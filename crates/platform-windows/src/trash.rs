//! Moves items to the Recycle Bin through `IFileOperation`.
//!
//! Asking for recycling is not enough on its own. Items the Recycle Bin can't take,
//! such as ones over its size limit, get deleted for good with no prompt when
//! running silently. So a progress sink watches each delete and aborts unless
//! Windows says it will recycle. There is no path through here that destroys data.

use std::cell::{Cell, RefCell};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gigsift_core::RecycleError;
use windows::Win32::Foundation::E_ABORT;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FOF_ALLOWUNDO, FOF_NO_UI, FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation,
    IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
    SHCreateItemFromParsingName, SIGDN_FILESYSPATH, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows_core::{HRESULT, PCWSTR, Ref, Result, implement};
use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

/// `GetDriveTypeW` result for a network drive, from `winbase.h`.
const DRIVE_REMOTE: u32 = 4;

#[derive(Default)]
struct Outcome {
    refused: Cell<bool>,
    location: RefCell<Option<PathBuf>>,
}

#[implement(IFileOperationProgressSink)]
struct RecycleOnly {
    outcome: Rc<Outcome>,
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for RecycleOnly_Impl {
    fn PreDeleteItem(&self, dwflags: u32, _item: Ref<IShellItem>) -> Result<()> {
        if dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 != 0 {
            return Ok(());
        }
        self.outcome.refused.set(true);
        Err(E_ABORT.into())
    }

    fn PostDeleteItem(
        &self,
        _dwflags: u32,
        _item: Ref<IShellItem>,
        hrdelete: HRESULT,
        psinewlycreated: Ref<IShellItem>,
    ) -> Result<()> {
        if let (true, Some(item)) = (hrdelete.is_ok(), psinewlycreated.as_ref()) {
            // SAFETY: `item` is live for this call. The string is ours to free.
            unsafe {
                let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
                let text = name.to_string();
                CoTaskMemFree(Some(name.0 as *const _));
                *self.outcome.location.borrow_mut() = text.ok().map(PathBuf::from);
            }
        }
        Ok(())
    }

    fn StartOperations(&self) -> Result<()> {
        Ok(())
    }

    fn FinishOperations(&self, _hrresult: HRESULT) -> Result<()> {
        Ok(())
    }

    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        Ok(())
    }

    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> Result<()> {
        Ok(())
    }

    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    fn UpdateProgress(&self, _: u32, _: u32) -> Result<()> {
        Ok(())
    }

    fn ResetTimer(&self) -> Result<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> Result<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> Result<()> {
        Ok(())
    }
}

/// Balances `CoInitializeEx` on every path out.
struct ComApartment;

impl ComApartment {
    fn enter() -> Result<Self> {
        // SAFETY: called once per use, and undone by `Drop` on this thread.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()? };
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful `CoInitializeEx` in `enter`.
        unsafe { CoUninitialize() };
    }
}

/// Moves `path` to the Recycle Bin and returns where it went, or leaves it alone.
pub fn move_to_trash(path: &Path) -> std::result::Result<Option<PathBuf>, RecycleError> {
    if is_network_path(path) {
        return Err(RecycleError::Unavailable(
            "network drives have no Recycle Bin".to_owned(),
        ));
    }
    let outcome = Rc::new(Outcome::default());
    let performed = recycle(path, &outcome);
    if outcome.refused.get() {
        return Err(RecycleError::Unavailable(
            "Windows could not recycle this item, so it was left in place".to_owned(),
        ));
    }
    performed.map_err(|error| RecycleError::Failed(error.message()))?;
    if path.exists() {
        return Err(RecycleError::Failed(
            "Windows reported success but the item is still there".to_owned(),
        ));
    }
    Ok(outcome.location.take())
}

fn recycle(path: &Path, outcome: &Rc<Outcome>) -> Result<()> {
    let _apartment = ComApartment::enter()?;
    // The shell doesn't parse `\\?\` paths, so this one stays unprefixed. A path
    // too long for the shell fails here, and is left where it is.
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and outlives each call. COM objects are
    // released on drop, while the apartment is still open.
    unsafe {
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        operation.SetOperationFlags(
            FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOFX_EARLYFAILURE,
        )?;
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)?;
        let sink: IFileOperationProgressSink = RecycleOnly {
            outcome: Rc::clone(outcome),
        }
        .into();
        let cookie = operation.Advise(&sink)?;
        operation.DeleteItem(&item, None)?;
        let performed = operation.PerformOperations();
        operation.Unadvise(cookie)?;
        performed
    }
}

fn is_network_path(path: &Path) -> bool {
    let Some(root) = path.ancestors().last() else {
        return false;
    };
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated.
    unsafe { GetDriveTypeW(wide.as_ptr()) == DRIVE_REMOTE }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real Recycle Bin, so only CI runners opt in.
    #[test]
    fn a_folder_lands_in_the_recycle_bin() {
        if std::env::var_os("GIGSIFT_TEST_REAL_TRASH").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("gigsift-trash-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner/file.bin"), vec![7u8; 4096]).unwrap();

        let location = move_to_trash(&dir).unwrap();

        assert!(!dir.exists());
        let location = location.expect("Windows should say where it went");
        assert!(location.exists(), "{}", location.display());
    }
}
