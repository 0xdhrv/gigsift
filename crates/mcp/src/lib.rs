//! `gigsift mcp serve`, an MCP server over stdio.
//!
//! An agent can look and can propose, but never act: there is deliberately no tool
//! here that applies a plan or removes anything. A person reviews what an agent
//! planned and applies it with the CLI or Raycast.

use std::error::Error;
use std::path::PathBuf;

use gigsift_core::age::parse_age;
use gigsift_core::size::parse_size;
use gigsift_core::{PathSharing, Profile, redact};
use gigsift_engine::{
    Analyzer, create_plan, explain, find_candidates, find_projects, largest_entries,
    storage_summary,
};
use gigsift_recipes::RecipeSet;
use gigsift_store::Store;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const DEFAULT_ACTIVE_WITHIN: &str = "14d";
const DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct SummaryArgs {
    /// Folder to scan. Defaults to the home folder.
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct LargeFilesArgs {
    /// Folder to scan. Defaults to the home folder.
    path: Option<String>,
    /// Smallest size to include, like `500mb` or `1gib`. Defaults to `100mb`.
    min_size: Option<String>,
    /// Most entries to return. Defaults to 50.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExplainArgs {
    /// File or folder to explain.
    path: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct CandidatesArgs {
    /// Folder to search. Defaults to the home folder.
    path: Option<String>,
    /// Count a project as active if it changed this recently, like `14d`.
    active_within: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct ProjectsArgs {
    /// Folder to search. Defaults to the home folder.
    path: Option<String>,
    /// Only list projects idle at least this long, like `30d`.
    stale: Option<String>,
    /// Count a project as active if it changed this recently, like `14d`.
    active_within: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PlanArgs {
    /// How much to recover, like `10gb`.
    target: String,
    /// Folder to search. Defaults to the home folder.
    path: Option<String>,
    /// Count a project as active if it changed this recently, like `14d`.
    active_within: Option<String>,
}

#[derive(Clone)]
pub struct GigsiftServer {
    sharing: PathSharing,
}

fn root_or_home(path: Option<String>) -> Result<PathBuf, String> {
    match path {
        Some(path) => Ok(PathBuf::from(path)),
        None => std::env::home_dir().ok_or_else(|| "no home folder found".to_owned()),
    }
}

fn window(active_within: Option<String>) -> Result<u64, String> {
    parse_age(active_within.as_deref().unwrap_or(DEFAULT_ACTIVE_WITHIN))
}

impl GigsiftServer {
    /// Runs blocking work off the async runtime, then shares only what's allowed.
    async fn respond<T, F>(&self, work: F) -> Result<String, String>
    where
        T: Serialize,
        F: FnOnce() -> Result<T, String> + Send + 'static,
    {
        let sharing = self.sharing;
        tokio::task::spawn_blocking(move || {
            let mut value = serde_json::to_value(work()?).map_err(|e| e.to_string())?;
            redact(&mut value, sharing, std::env::home_dir().as_deref());
            serde_json::to_string(&value).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

#[tool_router(server_handler = false)]
impl GigsiftServer {
    #[tool(
        description = "Total disk use of a folder: logical and on-disk bytes, file counts, \
        and its largest subfolders. Read-only."
    )]
    async fn get_storage_summary(
        &self,
        Parameters(args): Parameters<SummaryArgs>,
    ) -> Result<String, String> {
        self.respond(move || storage_summary(&root_or_home(args.path)?).map_err(|e| e.to_string()))
            .await
    }

    #[tool(description = "The largest files and folders under a path. Read-only.")]
    async fn find_large_files(
        &self,
        Parameters(args): Parameters<LargeFilesArgs>,
    ) -> Result<String, String> {
        self.respond(move || {
            let root = root_or_home(args.path)?;
            let min_size = parse_size(args.min_size.as_deref().unwrap_or("100mb"))?;
            let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
            largest_entries(&root, min_size, limit).map_err(|e| e.to_string())
        })
        .await
    }

    #[tool(
        description = "What a path is, which project it belongs to, what removing it would \
        lose, how to rebuild it, and a safety verdict. Read-only. An unknown verdict does not \
        mean a path is disposable."
    )]
    async fn explain_storage_path(
        &self,
        Parameters(args): Parameters<ExplainArgs>,
    ) -> Result<String, String> {
        self.respond(move || {
            let recipes = RecipeSet::builtin();
            let analyzer = Analyzer::for_user(&recipes, window(None)?);
            explain(&PathBuf::from(args.path), &analyzer).map_err(|e| e.to_string())
        })
        .await
    }

    #[tool(
        description = "Everything that could be recovered right now, with why each item is \
        removable, what is lost, and how to rebuild it. Also lists what safety checks blocked. \
        Read-only: use create_cleanup_plan to save a plan for a person to review."
    )]
    async fn find_cleanup_candidates(
        &self,
        Parameters(args): Parameters<CandidatesArgs>,
    ) -> Result<String, String> {
        self.respond(move || {
            let root = root_or_home(args.path)?;
            let recipes = RecipeSet::builtin();
            let analyzer = Analyzer::for_user(&recipes, window(args.active_within)?);
            find_candidates(&root, &analyzer).map_err(|e| e.to_string())
        })
        .await
    }

    #[tool(
        description = "Development projects holding reclaimable artifacts, with their \
        activity, git state, and what is recoverable from each. Read-only."
    )]
    async fn get_project_storage(
        &self,
        Parameters(args): Parameters<ProjectsArgs>,
    ) -> Result<String, String> {
        self.respond(move || {
            let root = root_or_home(args.path)?;
            let stale = args.stale.as_deref().map(parse_age).transpose()?;
            let recipes = RecipeSet::builtin();
            let analyzer = Analyzer::for_user(&recipes, window(args.active_within)?);
            find_projects(&root, stale, &analyzer).map_err(|e| e.to_string())
        })
        .await
    }

    #[tool(
        description = "Saves a cleanup plan that reaches a target size. This changes no \
        files. It returns a plan_id: tell the user to review it with `gigsift clean show` and \
        apply it themselves with `gigsift clean apply`. No tool here can apply a plan."
    )]
    async fn create_cleanup_plan(
        &self,
        Parameters(args): Parameters<PlanArgs>,
    ) -> Result<String, String> {
        self.respond(move || {
            let root = root_or_home(args.path)?;
            let target = parse_size(&args.target)?;
            let recipes = RecipeSet::builtin();
            let analyzer = Analyzer::for_user(&recipes, window(args.active_within)?);
            let plan = create_plan(&root, target, Profile::Developer, &analyzer)
                .map_err(|e| e.to_string())?;
            Store::for_user()
                .and_then(|store| store.save_plan(&plan))
                .map_err(|e| e.to_string())?;
            Ok(plan)
        })
        .await
    }
}

#[tool_handler(
    name = "gigsift",
    // Must be a literal here. A test keeps it equal to the package version.
    version = "0.2.0",
    instructions = "Explains local disk usage and proposes cleanups. These tools only \
    read, except create_cleanup_plan, which saves a plan for review. Nothing here can remove \
    anything: to act on a plan, a person runs `gigsift clean apply <plan_id>` themselves. Sizes \
    are bytes. Always report scan_completeness and warnings alongside totals, because a partial \
    scan means totals are a lower bound. A safety verdict of unknown does not mean disposable."
)]
impl ServerHandler for GigsiftServer {}

/// Serves until the client disconnects. Stdout carries only the protocol.
pub fn serve(sharing: PathSharing) -> Result<(), Box<dyn Error>> {
    tokio::runtime::Runtime::new()?.block_on(async move {
        let server = GigsiftServer { sharing };
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}
