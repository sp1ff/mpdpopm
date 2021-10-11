extern crate lalrpop;
fn main() {
    lalrpop::Configuration::new()
        .emit_comments(true)
        .emit_whitespace(true)
        .log_verbose()
        .process_current_dir()
        .unwrap();
}
