use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TasksQuery {
    pub commands: Vec<TasksQueryCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TasksQueryCommand {
    Filter { filter: TasksFilter },
    Sort { field: String, reverse: bool },
    Group { field: String, reverse: bool },
    Limit { value: usize },
    LimitGroups { value: usize },
    Hide { field: String },
    Show { field: String },
    ShortMode,
    Explain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TasksFilter {
    Done {
        value: bool,
    },
    StatusNameIncludes {
        value: String,
    },
    StatusTypeIs {
        value: String,
    },
    Date {
        field: TasksDateField,
        relation: TasksDateRelation,
        value: String,
    },
    HasDate {
        field: TasksDateField,
        value: bool,
    },
    TextIncludes {
        field: TasksTextField,
        value: String,
    },
    TagIncludes {
        value: String,
    },
    PriorityIs {
        value: String,
    },
    Recurring {
        value: bool,
    },
    Blocked {
        value: bool,
    },
    HasId,
    Not {
        filter: Box<TasksFilter>,
    },
    And {
        filters: Vec<TasksFilter>,
    },
    Or {
        filters: Vec<TasksFilter>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TasksDateField {
    Due,
    Created,
    Start,
    Scheduled,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TasksDateRelation {
    Before,
    After,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TasksTextField {
    Description,
    Path,
    Heading,
}
