use super::{PreparedTemplateInsertion, TemplateInsertMode, TemplateInsertionError};
use crate::templates::{YamlMapping, YamlValue};

pub fn prepare_template_insertion(
    target_source: &str,
    rendered_template: &str,
) -> Result<PreparedTemplateInsertion, TemplateInsertionError> {
    let (target_frontmatter, target_body) = parse_frontmatter_document(target_source, false)?;
    let (template_frontmatter, template_body) =
        parse_frontmatter_document(rendered_template, true)?;

    Ok(PreparedTemplateInsertion {
        merged_frontmatter: merge_template_frontmatter(target_frontmatter, template_frontmatter),
        target_body,
        template_body,
    })
}

pub fn parse_frontmatter_document(
    source: &str,
    template_document: bool,
) -> Result<(Option<YamlMapping>, String), TemplateInsertionError> {
    let Some((yaml_start, yaml_end, body_start)) = find_frontmatter_block(source) else {
        return Ok((None, source.to_string()));
    };

    let raw_yaml = &source[yaml_start..yaml_end];
    let value = serde_yaml::from_str::<YamlValue>(raw_yaml).map_err(|error| {
        if template_document {
            TemplateInsertionError::TemplateFrontmatterParse(error)
        } else {
            TemplateInsertionError::NoteFrontmatterParse(error)
        }
    })?;
    let mapping = value.as_mapping().cloned().ok_or({
        if template_document {
            TemplateInsertionError::TemplateFrontmatterNotMapping
        } else {
            TemplateInsertionError::NoteFrontmatterNotMapping
        }
    })?;

    Ok((Some(mapping), source[body_start..].to_string()))
}

#[must_use]
pub fn merge_template_frontmatter(
    target_frontmatter: Option<YamlMapping>,
    template_frontmatter: Option<YamlMapping>,
) -> Option<YamlMapping> {
    match (target_frontmatter, template_frontmatter) {
        (None, None) => None,
        (Some(target), None) => Some(target),
        (None, Some(template)) => Some(template),
        (Some(mut target), Some(template)) => {
            for (key, template_value) in template {
                if let Some(existing_value) = target.get_mut(&key) {
                    merge_template_property_value(existing_value, &template_value);
                } else {
                    target.insert(key, template_value);
                }
            }
            Some(target)
        }
    }
}

fn merge_template_property_value(existing: &mut YamlValue, template: &YamlValue) {
    if let (Some(existing_items), Some(template_items)) =
        (existing.as_sequence_mut(), template.as_sequence())
    {
        for template_item in template_items {
            if !existing_items.iter().any(|item| item == template_item) {
                existing_items.push(template_item.clone());
            }
        }
    }
}

pub fn render_note_from_parts(
    frontmatter: Option<&YamlMapping>,
    body: &str,
) -> Result<String, TemplateInsertionError> {
    let mut rendered = String::new();
    if let Some(frontmatter) = frontmatter {
        rendered.push_str(&format_frontmatter_block(frontmatter)?);
    }
    rendered.push_str(body);
    Ok(rendered)
}

pub fn apply_template_insertion_mode(
    prepared: &PreparedTemplateInsertion,
    mode: TemplateInsertMode,
) -> Result<String, TemplateInsertionError> {
    let body = match mode {
        TemplateInsertMode::Append => {
            append_template_body(&prepared.target_body, &prepared.template_body)
        }
        TemplateInsertMode::Prepend => {
            prepend_template_body(&prepared.target_body, &prepared.template_body)
        }
    };

    render_note_from_parts(prepared.merged_frontmatter.as_ref(), &body)
}

fn append_template_body(target_body: &str, template_body: &str) -> String {
    merge_body_sections(target_body, template_body, false)
}

fn prepend_template_body(target_body: &str, template_body: &str) -> String {
    merge_body_sections(template_body, target_body, true)
}

fn merge_body_sections(first: &str, second: &str, preserve_second_leading_space: bool) -> String {
    let first = first.trim_end_matches('\n');
    let second = if preserve_second_leading_space {
        second.trim_end_matches('\n')
    } else {
        second.trim_matches('\n')
    };

    match (first.is_empty(), second.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{first}\n"),
        (true, false) => format!("{second}\n"),
        (false, false) => format!("{first}\n\n{second}\n"),
    }
}

pub fn format_frontmatter_block(
    frontmatter: &YamlMapping,
) -> Result<String, TemplateInsertionError> {
    let mut yaml = serde_yaml::to_string(&YamlValue::Mapping(frontmatter.clone()))
        .map_err(TemplateInsertionError::YamlSerialize)?;
    if let Some(stripped) = yaml.strip_prefix("---\n") {
        yaml = stripped.to_string();
    }
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    Ok(format!("---\n{yaml}---\n"))
}

#[must_use]
pub fn find_frontmatter_block(source: &str) -> Option<(usize, usize, usize)> {
    let mut lines = source.split_inclusive('\n');
    let first_line = lines.next()?;
    if !matches!(first_line, "---\n" | "---\r\n" | "---") {
        return None;
    }

    let yaml_start = first_line.len();
    let mut offset = yaml_start;
    for line in lines {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            return Some((yaml_start, offset, offset + line.len()));
        }
        offset += line.len();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_document_extracts_yaml_and_body() {
        let (frontmatter, body) = parse_frontmatter_document("---\ntags:\n- a\n---\nBody\n", false)
            .expect("frontmatter should parse");
        let frontmatter = frontmatter.expect("frontmatter should exist");

        assert_eq!(
            frontmatter
                .get(YamlValue::String("tags".to_string()))
                .and_then(YamlValue::as_sequence)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(body, "Body\n");
    }

    #[test]
    fn merge_template_frontmatter_appends_unique_sequence_items() {
        let (target, _) =
            parse_frontmatter_document("---\ntags:\n- a\n---\n", false).expect("target");
        let (template, _) =
            parse_frontmatter_document("---\ntags:\n- a\n- b\n---\n", true).expect("template");

        let merged = merge_template_frontmatter(target, template).expect("merged");
        let tags = merged
            .get(YamlValue::String("tags".to_string()))
            .and_then(YamlValue::as_sequence)
            .expect("tags");

        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn prepare_template_insertion_merges_frontmatter_and_appends_body() {
        let prepared = prepare_template_insertion(
            "---\nstatus: old\n---\nExisting\n",
            "---\ntags:\n- new\n---\nInserted\n",
        )
        .expect("prepared");
        let rendered =
            apply_template_insertion_mode(&prepared, TemplateInsertMode::Append).expect("rendered");

        assert!(rendered.contains("status: old"));
        assert!(rendered.contains("tags:"));
        assert!(rendered.ends_with("Existing\n\nInserted\n"));
    }
}
