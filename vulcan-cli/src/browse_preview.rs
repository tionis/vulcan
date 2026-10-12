//! Background evaluation of browse's rendered note preview.
//!
//! Evaluating a note can run slow Dataview queries or `DataviewJS`, so a
//! worker thread owns the renderer and the UI never waits on it. Requests
//! carry a generation; when several queue up while the worker is busy, only
//! the newest is rendered, and the UI ignores results for notes it has
//! already moved past.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use vulcan_core::{
    DataviewJsRenderPolicy, EvaluatedMarkdownOptions, EvaluatedMarkdownRenderer, PermissionGuard,
    ProfilePermissionGuard, VaultPaths,
};

enum PreviewRequest {
    Render {
        generation: u64,
        path: String,
    },
    /// Drop the loaded note index, for example after a scan or an edit.
    Reset,
}

/// One note's evaluated Markdown.
#[derive(Debug)]
pub(crate) struct RenderedNote {
    pub(crate) generation: u64,
    pub(crate) path: String,
    pub(crate) markdown: String,
}

#[derive(Debug)]
pub(crate) struct PreviewWorker {
    requests: Sender<PreviewRequest>,
    results: Receiver<RenderedNote>,
    generation: u64,
}

impl PreviewWorker {
    /// Start a worker that renders under `guard` (a restricted profile's
    /// read authority) and hands `profile` to `DataviewJS`.
    pub(crate) fn spawn(
        paths: VaultPaths,
        guard: Option<ProfilePermissionGuard>,
        profile: Option<String>,
    ) -> Self {
        let (requests, request_receiver) = mpsc::channel();
        let (result_sender, results) = mpsc::channel();
        thread::spawn(move || {
            run_worker(
                &paths,
                guard.as_ref(),
                profile.as_deref(),
                &request_receiver,
                &result_sender,
            );
        });
        Self {
            requests,
            results,
            generation: 0,
        }
    }

    /// Ask for `path` to be rendered; earlier requests become stale.
    pub(crate) fn request(&mut self, path: &str) {
        self.generation += 1;
        let _ = self.requests.send(PreviewRequest::Render {
            generation: self.generation,
            path: path.to_string(),
        });
    }

    pub(crate) fn reset(&self) {
        let _ = self.requests.send(PreviewRequest::Reset);
    }

    /// The result of the latest request, once it is ready.
    pub(crate) fn try_take(&self) -> Option<RenderedNote> {
        let mut latest = None;
        loop {
            match self.results.try_recv() {
                Ok(note) if note.generation == self.generation => latest = Some(note),
                Ok(_) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return latest,
            }
        }
    }

    /// Block until the latest request is rendered.
    #[cfg(test)]
    pub(crate) fn wait(&self) -> Option<RenderedNote> {
        loop {
            let note = self
                .results
                .recv_timeout(std::time::Duration::from_secs(60))
                .ok()?;
            if note.generation == self.generation {
                return Some(note);
            }
        }
    }
}

fn run_worker(
    paths: &VaultPaths,
    guard: Option<&ProfilePermissionGuard>,
    profile: Option<&str>,
    requests: &Receiver<PreviewRequest>,
    results: &Sender<RenderedNote>,
) {
    let guard = guard.map(|guard| guard as &dyn PermissionGuard);
    let mut renderer = None::<(EvaluatedMarkdownRenderer, DataviewJsRenderPolicy)>;
    while let Ok(first) = requests.recv() {
        // Coalesce everything queued while the last note rendered.
        let mut latest = None;
        for request in std::iter::once(first).chain(requests.try_iter()) {
            match request {
                PreviewRequest::Reset => renderer = None,
                PreviewRequest::Render { generation, path } => latest = Some((generation, path)),
            }
        }
        let Some((generation, path)) = latest else {
            continue;
        };
        let (renderer, dataview_js) = renderer.get_or_insert_with(|| {
            (
                EvaluatedMarkdownRenderer::load(paths, guard),
                vulcan_core::vault_dataview_js_policy(paths, None),
            )
        });
        let markdown = render_note(paths, renderer, *dataview_js, guard, profile, &path);
        if results
            .send(RenderedNote {
                generation,
                path,
                markdown,
            })
            .is_err()
        {
            return;
        }
    }
}

/// The note's Markdown with Dataview, Tasks, Bases, and note embeds
/// evaluated under the session's read authority. `DataviewJS` follows the
/// vault's policy: the strict sandbox, or full evaluation (read-only) in a
/// trusted vault.
fn render_note(
    paths: &VaultPaths,
    renderer: &EvaluatedMarkdownRenderer,
    dataview_js: DataviewJsRenderPolicy,
    guard: Option<&dyn PermissionGuard>,
    profile: Option<&str>,
    path: &str,
) -> String {
    let source = match std::fs::read_to_string(paths.vault_root().join(path)) {
        Ok(source) => source,
        Err(error) => return format!("> [!danger] Could not read {path}\n> {error}"),
    };
    renderer
        .render(
            &source,
            &EvaluatedMarkdownOptions {
                source_path: Some(path),
                dataview_js,
                guard,
                permission_profile: profile,
            },
        )
        .markdown
}
