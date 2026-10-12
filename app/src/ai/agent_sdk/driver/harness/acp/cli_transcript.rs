//! The CLI behind an ACP adapter writes the same session transcript it writes when run in a
//! terminal, so the terminal runners' capture code applies unchanged once that session is
//! identified. ACP treats session ids as opaque, so how an adapter's session id relates to the
//! CLI's is decided per harness in [`CliTranscriptKind::cli_session_id`]. A transcript that
//! cannot be identified costs the run its CLI transcript upload and nothing else.
//!
//! One gap: when claude-agent-acp restarts a session to clear its context after a plan is
//! approved, the SDK continues under a fresh id that the adapter never reports. Captures keep
//! reading the transcript under the ACP session id, which stops at the restart.
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde_json::Value;
use uuid::Uuid;
use walkdir::WalkDir;
use warp_cli::agent::Harness;

use super::super::claude_transcript::{claude_config_dir, session_transcript_path};
use super::super::codex_transcript::{codex_sessions_root, find_session_file};
use super::super::transcript_persistence::{CapturedTranscript, UploadedTranscriptUsage};
use super::super::usage_reporting::CaptureIdentity;
use super::super::{claude_code, codex};

/// The CLI transcript format an ACP harness produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CliTranscriptKind {
    Codex,
    Claude,
    Pi,
}

impl CliTranscriptKind {
    /// The harnesses whose CLI transcript is uploaded for an ACP run.
    pub(super) fn for_harness(harness: Harness) -> Option<Self> {
        match harness {
            Harness::Codex => Some(Self::Codex),
            Harness::Claude => Some(Self::Claude),
            Harness::Pi => Some(Self::Pi),
            Harness::Gemini | Harness::Oz | Harness::OpenCode | Harness::Unknown => None,
        }
    }

    /// The CLI's own session id behind an ACP session, which names its transcript on disk.
    /// `None` when the adapter's id cannot be mapped to one.
    pub(super) fn cli_session_id(self, acp_session_id: &str) -> Option<Uuid> {
        match self {
            // codex-acp returns the Codex thread id from `session/new`, and claude-agent-acp
            // passes its session id to the Agent SDK as the Claude session id.
            Self::Codex | Self::Claude => Uuid::parse_str(acp_session_id).ok(),
            // pi-acp mints its own id and records which native session it maps to; `locate`
            // resolves the file through that map, so the id is only an identity here.
            Self::Pi => Uuid::parse_str(acp_session_id).ok(),
        }
    }

    /// The directory the CLI keeps its transcripts under, from the same environment the adapter
    /// inherits.
    pub(super) fn root(self) -> Result<PathBuf> {
        match self {
            Self::Codex => codex_sessions_root(),
            Self::Claude => claude_config_dir(),
            Self::Pi => std::env::var_os("PI_CODING_AGENT_DIR")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("PI_CODING_AGENT_DIR is not configured")),
        }
    }

    /// The transcript file, once the CLI has written it. `root` is [`Self::root`].
    pub(super) fn locate(self, root: &Path, cwd: &Path, session_id: Uuid) -> Option<PathBuf> {
        match self {
            Self::Codex => find_session_file(root, session_id),
            Self::Claude => {
                Some(session_transcript_path(root, cwd, session_id)).filter(|path| path.exists())
            }
            Self::Pi => locate_pi_transcript(root, cwd, session_id),
        }
    }

    /// Captures native transcript bytes and any supported usage data.
    pub(super) fn capture(
        self,
        session_id: Uuid,
        path: &Path,
        root: &Path,
        cwd: &Path,
        is_final: bool,
        identity: Option<CaptureIdentity>,
    ) -> Result<CapturedTranscript> {
        match self {
            Self::Codex => {
                codex::capture_transcript_with_usage(session_id, path, is_final, identity)
            }
            Self::Claude => claude_code::capture_transcript_with_usage(
                session_id, cwd, root, None, is_final, identity,
            ),
            Self::Pi => Ok(CapturedTranscript {
                transcript_body: fs::read(path)?,
                usage_request: None,
                needs_retry: false,
            }),
        }
    }

    /// The save result when the transcript does not exist. The CLI writes it only after the
    /// first prompt, so its absence during the run is expected. Its absence at the final save is
    /// not, but the CLI transcript supplements the native conversation the run already uploads,
    /// so it never fails the save.
    pub(super) fn missing_transcript(self, is_final: bool) -> UploadedTranscriptUsage {
        if is_final {
            log::warn!(
                "{self:?} transcript still unavailable at final save; it was never uploaded"
            );
        } else {
            log::debug!("{self:?} transcript for the ACP session is not available yet");
        }
        UploadedTranscriptUsage::empty()
    }
}

fn locate_pi_transcript(root: &Path, cwd: &Path, session_id: Uuid) -> Option<PathBuf> {
    let session_dir = root.join("sessions");
    let map = dirs::home_dir()
        .map(|home| home.join(".pi/pi-acp/session-map.json"))
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    if let Some(map) = map.filter(|map| map["version"] == 1)
        && let Some(entry) = map["sessions"].get(session_id.to_string())
        && let Some(path) = entry["sessionFile"].as_str().map(PathBuf::from)
        && let Ok(path) = path.canonicalize()
        && let Ok(session_dir) = session_dir.canonicalize()
        && path.starts_with(session_dir)
        && pi_transcript_header(&path, cwd).is_some()
    {
        return Some(path);
    }
    // pi-acp keeps its map under HOME even when Pi's session directory is overridden.
    // Restrict fallback discovery to this run's scratch tree, never the user's global sessions.
    let suffix = format!("_{session_id}.jsonl");
    WalkDir::new(&session_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "jsonl"))
        .filter_map(|entry| pi_transcript_header(entry.path(), cwd).map(|id| (entry, id)))
        .max_by_key(|(entry, id)| {
            (
                *id == session_id,
                entry.file_name().to_string_lossy().ends_with(&suffix),
                entry.metadata().ok().and_then(|metadata| metadata.modified().ok()),
            )
        })
        .map(|(entry, _)| entry.into_path())
}

fn pi_transcript_header(path: &Path, cwd: &Path) -> Option<Uuid> {
    let file = File::open(path).ok()?;
    let mut header = String::new();
    if BufReader::new(file.take(8 * 1024 * 1024))
        .read_line(&mut header)
        .is_err()
    {
        return None;
    }
    let header = serde_json::from_str::<Value>(&header).ok()?;
    if header["type"] != "session"
        || !header["cwd"].as_str().is_some_and(|path| Path::new(path) == cwd)
    {
        return None;
    }
    header["id"].as_str().and_then(|id| Uuid::parse_str(id).ok())
}

#[cfg(test)]
#[path = "cli_transcript_tests.rs"]
mod tests;
