use pulldown_cmark::Options;

#[must_use]
pub fn parser_options() -> Options {
    parser_options_internal(true)
}

#[must_use]
pub fn fragment_parser_options() -> Options {
    parser_options_internal(false)
}

fn parser_options_internal(include_metadata_blocks: bool) -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_WIKILINKS);
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_FOOTNOTES);
    if include_metadata_blocks {
        options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    }
    options
}
