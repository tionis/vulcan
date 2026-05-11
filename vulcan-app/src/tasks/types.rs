use serde::{Deserialize, Serialize};
use serde_json::Value;
use vulcan_core::config::TasksDefaultSource;
use vulcan_core::{ParsedTaskNoteInput, RefactorChange, TasksQueryResult};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskMutationReport {
    pub action: String,
    pub dry_run: bool,
    pub path: String,
    pub moved_from: Option<String>,
    pub moved_to: Option<String>,
    pub changes: Vec<RefactorChange>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskSetRequest {
    pub task: String,
    pub property: String,
    pub value: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskRescheduleRequest {
    pub task: String,
    pub due: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskCompleteRequest {
    pub task: String,
    pub date: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskArchiveRequest {
    pub task: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskAddRequest {
    pub text: String,
    pub no_nlp: bool,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub template: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskAddReport {
    pub action: String,
    pub dry_run: bool,
    pub created: bool,
    pub used_nlp: bool,
    pub path: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub time_estimate: Option<usize>,
    pub recurrence: Option<String>,
    pub template: Option<String>,
    pub frontmatter: Value,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_input: Option<ParsedTaskNoteInput>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskCreateRequest {
    pub text: String,
    pub note: Option<String>,
    pub due: Option<String>,
    pub priority: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskCreateReport {
    pub action: String,
    pub dry_run: bool,
    pub path: String,
    pub task: String,
    pub created_note: bool,
    pub line_number: i64,
    pub used_nlp: bool,
    pub line: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub priority: Option<String>,
    pub recurrence: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub changes: Vec<RefactorChange>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskConvertRequest {
    pub file: String,
    pub line: Option<i64>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskConvertReport {
    pub action: String,
    pub dry_run: bool,
    pub mode: String,
    pub source_path: String,
    pub target_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<i64>,
    pub title: String,
    pub created: bool,
    pub source_changes: Vec<RefactorChange>,
    pub task_changes: Vec<RefactorChange>,
    pub frontmatter: Value,
    pub body: String,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskShowReport {
    pub path: String,
    pub title: String,
    pub status: String,
    pub status_type: String,
    pub completed: bool,
    pub archived: bool,
    pub priority: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub completed_date: Option<String>,
    pub date_created: Option<String>,
    pub date_modified: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub recurrence: Option<String>,
    pub recurrence_anchor: Option<String>,
    pub complete_instances: Vec<String>,
    pub skipped_instances: Vec<String>,
    pub blocked_by: Vec<Value>,
    pub reminders: Vec<Value>,
    pub time_entries: Vec<Value>,
    pub total_time_minutes: i64,
    pub active_time_minutes: i64,
    pub estimate_remaining_minutes: Option<i64>,
    pub efficiency_ratio: Option<i64>,
    pub custom_fields: Value,
    pub frontmatter: Value,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskDueReport {
    pub reference_time: String,
    pub within: String,
    pub tasks: Vec<TaskDueItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskDueItem {
    pub path: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub due: String,
    pub overdue: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskRemindersReport {
    pub reference_time: String,
    pub upcoming: String,
    pub reminders: Vec<TaskReminderItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskReminderItem {
    pub path: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub reminder_id: String,
    pub reminder_type: String,
    pub related_to: Option<String>,
    pub description: Option<String>,
    pub notify_at: String,
    pub overdue: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvalRequest {
    pub file: String,
    pub block: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksEvalReport {
    pub file: String,
    pub blocks: Vec<TasksBlockEvalReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksBlockEvalReport {
    pub block_index: usize,
    pub line_number: i64,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_source: Option<String>,
    pub result: Option<TasksQueryResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListRequest {
    pub filter: Option<String>,
    pub source: Option<TasksDefaultSource>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub due_before: Option<String>,
    pub due_after: Option<String>,
    pub project: Option<String>,
    pub context: Option<String>,
    pub group_by: Option<String>,
    pub sort_by: Option<String>,
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskNotesViewListItem {
    pub file: String,
    pub file_stem: String,
    pub view_name: Option<String>,
    pub view_type: String,
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskNotesViewListReport {
    pub views: Vec<TaskNotesViewListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksNextReport {
    pub reference_date: String,
    pub result_count: usize,
    pub occurrences: Vec<TasksNextOccurrence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksNextOccurrence {
    pub date: String,
    pub sequence: usize,
    pub task: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksBlockedReport {
    pub tasks: Vec<TasksBlockedItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksBlockedItem {
    pub task: Value,
    pub blockers: Vec<TaskDependencyEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TasksGraphReport {
    pub nodes: Vec<TaskDependencyNode>,
    pub edges: Vec<TaskDependencyEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskDependencyNode {
    pub key: String,
    pub id: Option<String>,
    pub path: String,
    pub line: i64,
    pub text: String,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskDependencyEdge {
    pub blocked_key: String,
    pub blocker_id: String,
    pub relation_type: Option<String>,
    pub gap: Option<String>,
    pub resolved: bool,
    pub blocker_key: Option<String>,
    pub blocker_path: Option<String>,
    pub blocker_line: Option<i64>,
    pub blocker_text: Option<String>,
    pub blocker_completed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskTimeEntryReport {
    pub start_time: String,
    pub end_time: Option<String>,
    pub description: Option<String>,
    pub duration_minutes: i64,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct TaskTrackStartRequest {
    pub task: String,
    pub description: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskTrackStopRequest {
    pub task: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskTrackReport {
    pub action: String,
    pub dry_run: bool,
    pub path: String,
    pub title: String,
    pub session: TaskTimeEntryReport,
    pub total_time_minutes: i64,
    pub active_time_minutes: i64,
    pub estimate_remaining_minutes: Option<i64>,
    pub efficiency_ratio: Option<i64>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskTrackStatusItem {
    pub path: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub session: TaskTimeEntryReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskTrackStatusReport {
    pub active_sessions: Vec<TaskTrackStatusItem>,
    pub total_active_sessions: usize,
    pub total_elapsed_minutes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskTrackLogReport {
    pub path: String,
    pub title: String,
    pub total_time_minutes: i64,
    pub active_time_minutes: i64,
    pub estimate_remaining_minutes: Option<i64>,
    pub efficiency_ratio: Option<i64>,
    pub entries: Vec<TaskTimeEntryReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTrackSummaryPeriod {
    Day,
    Week,
    Month,
    All,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskTrackSummaryReport {
    pub period: String,
    pub from: String,
    pub to: String,
    pub total_minutes: i64,
    pub total_hours: f64,
    pub tasks_with_time: usize,
    pub active_tasks: usize,
    pub completed_tasks: usize,
    pub top_tasks: Vec<TaskTrackSummaryTaskItem>,
    pub top_projects: Vec<TaskTrackSummaryProjectItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskTrackSummaryTaskItem {
    pub path: String,
    pub title: String,
    pub minutes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskTrackSummaryProjectItem {
    pub project: String,
    pub minutes: i64,
}
