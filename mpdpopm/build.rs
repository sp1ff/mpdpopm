extern crate lalrpop;
fn main() {
    let mut cfg = lalrpop::Configuration::new();
    cfg.emit_comments(true)
        .emit_whitespace(true)
        .log_verbose()
        .process_current_dir()
        .unwrap();
}
