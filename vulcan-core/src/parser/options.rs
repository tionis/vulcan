use pulldown_cmark::Options;

#[must_use]
pub fn parser_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_WIKILINKS);
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    options
}
