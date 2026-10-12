//! Evaluated query results for display next to the source.
//!
//! Editors show the results of inline expressions, Dataview and Tasks
//! blocks, and `.base` embeds as virtual text beside the source rather than
//! replacing it. `DataviewJS` is the one region kind that runs code, so its
//! policy is clamped: an editor may ask for the strict `static` sandbox, but
//! `on` needs the device-local `[editor] dataview_js = "on"`, because opening
//! an untrusted vault must not run its scripts.

use super::{EditorRange, EditorSession};
use crate::AppError;
use vulcan_core::editor::LineIndex;
use vulcan_core::{
    DataviewJsRenderPolicy, EvaluatedMarkdownOptions, EvaluatedMarkdownRenderer, PermissionGuard,
    RenderDiagnostic, VaultPaths,
};

/// One evaluated region of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRenderRegion {
    pub range: EditorRange,
    /// `inline` or `block`.
    pub kind: &'static str,
    /// `inline`, `dataview`, `dataviewjs`, `tasks`, `base`, or `notice`.
    pub source: &'static str,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRender {
    pub regions: Vec<EditorRenderRegion>,
    pub diagnostics: Vec<RenderDiagnostic>,
    /// The `DataviewJS` policy the render actually used.
    pub dataview_js: DataviewJsRenderPolicy,
}

impl EditorSession {
    /// Evaluate the dynamic regions of `path` (the open buffer, else the file)
    /// that intersect `range`, or all of them.
    pub fn render(
        &self,
        path: &str,
        range: Option<EditorRange>,
        requested: DataviewJsRenderPolicy,
    ) -> Result<Option<EditorRender>, AppError> {
        let Some(view) = self.view(path) else {
            return Ok(None);
        };
        if let Some(guard) = self.guard() {
            guard
                .check_read_path(path)
                .map_err(|error| AppError::operation_with_code("permission_denied", error))?;
        }
        let dataview_js = effective_dataview_js(requested, local_dataview_js(self.paths()));
        let guard = self.guard().map(|guard| guard as &dyn PermissionGuard);
        let renderer = EvaluatedMarkdownRenderer::load(self.paths(), guard);
        let evaluated = renderer.render_regions(
            &view.text,
            &EvaluatedMarkdownOptions {
                source_path: Some(path),
                dataview_js,
                guard,
                permission_profile: self.permission_profile(),
            },
        );
        let wanted = range.map(|range| {
            let index = LineIndex::new(&view.text);
            index.offset(&view.text, range.start, self.encoding())
                ..index.offset(&view.text, range.end, self.encoding())
        });
        let regions = evaluated
            .regions
            .into_iter()
            .filter(|region| {
                wanted
                    .as_ref()
                    .is_none_or(|wanted| region.start <= wanted.end && wanted.start <= region.end)
            })
            .map(|region| EditorRenderRegion {
                range: self.range(&view.text, &view.line_index, region.start..region.end),
                kind: region.kind,
                source: region.source,
                markdown: region.markdown,
            })
            .collect();
        Ok(Some(EditorRender {
            regions,
            diagnostics: evaluated.diagnostics,
            dataview_js,
        }))
    }
}

/// The device-local `[editor] dataview_js` setting from
/// `.vulcan/config.local.toml`. The shared config is deliberately not
/// consulted: it syncs with the vault and is not the user's consent.
#[must_use]
pub fn local_dataview_js(paths: &VaultPaths) -> Option<DataviewJsRenderPolicy> {
    let text = std::fs::read_to_string(paths.local_config_file()).ok()?;
    let value = text.parse::<toml::Table>().ok()?;
    match value.get("editor")?.get("dataview_js")?.as_str()? {
        "off" => Some(DataviewJsRenderPolicy::Off),
        "static" => Some(DataviewJsRenderPolicy::Static),
        "on" => Some(DataviewJsRenderPolicy::On),
        _ => None,
    }
}

/// The policy an editor gets: what it asked for, except that `on` needs the
/// device-local setting (falling back to `static`), and a local `off`
/// disables `DataviewJS` entirely.
#[must_use]
pub fn effective_dataview_js(
    requested: DataviewJsRenderPolicy,
    local: Option<DataviewJsRenderPolicy>,
) -> DataviewJsRenderPolicy {
    match (requested, local) {
        (_, Some(DataviewJsRenderPolicy::Off)) | (DataviewJsRenderPolicy::Off, _) => {
            DataviewJsRenderPolicy::Off
        }
        (DataviewJsRenderPolicy::On, Some(DataviewJsRenderPolicy::On)) => {
            DataviewJsRenderPolicy::On
        }
        _ => DataviewJsRenderPolicy::Static,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use vulcan_core::editor::TextPosition;
    use DataviewJsRenderPolicy::{Off, On, Static};

    const HOME: &str = "---\nstatus: draft\n---\nStatus: `= this.status`\n\n```dataviewjs\ndv.paragraph(\"js ran\")\n```\n\n```tasks\nnot done\n```\n";

    #[test]
    fn clamps_dataview_js_to_local_consent() {
        assert_eq!(effective_dataview_js(Off, Some(On)), Off);
        assert_eq!(effective_dataview_js(Static, None), Static);
        assert_eq!(effective_dataview_js(On, None), Static);
        assert_eq!(effective_dataview_js(On, Some(Static)), Static);
        assert_eq!(effective_dataview_js(On, Some(On)), On);
        assert_eq!(effective_dataview_js(Static, Some(Off)), Off);

        let (dir, session) = vault(&[("Home.md", "x")]);
        assert_eq!(local_dataview_js(session.paths()), None);
        std::fs::write(
            dir.path().join(".vulcan/config.local.toml"),
            "[editor]\ndataview_js = \"on\"\n",
        )
        .unwrap();
        assert_eq!(local_dataview_js(session.paths()), Some(On));
    }

    #[test]
    fn renders_regions_of_open_buffers_within_a_range() {
        let (_dir, mut session) = vault(&[("Home.md", HOME), ("Todo.md", "- [ ] Open item\n")]);
        session.open("Home.md", 1, HOME.to_string());
        let render = session.render("Home.md", None, Off).unwrap().unwrap();
        assert_eq!(render.dataview_js, Off);
        let sources = render
            .regions
            .iter()
            .map(|region| region.source)
            .collect::<Vec<_>>();
        assert_eq!(sources, vec!["inline", "notice", "tasks"]);
        assert_eq!(render.regions[0].markdown, "draft");
        assert_eq!(
            render.regions[0].range.start,
            position_of(HOME, "`= this", 0)
        );
        assert!(render.regions[2].markdown.contains("Open item"));

        let tasks_line = position_of(HOME, "```tasks", 0);
        let only_tasks = session
            .render(
                "Home.md",
                Some(EditorRange {
                    start: tasks_line,
                    end: TextPosition {
                        line: tasks_line.line,
                        column: 3,
                    },
                }),
                Off,
            )
            .unwrap()
            .unwrap();
        assert_eq!(only_tasks.regions.len(), 1);
        assert_eq!(only_tasks.regions[0].source, "tasks");
        assert_eq!(session.render("Missing.md", None, Off).unwrap(), None);
    }
}
