#[path = "../gui_next/mod.rs"]
mod gui_next;
#[path = "../gui_shared.rs"]
mod gui_shared;

fn main() -> anyhow::Result<()> {
    let status = goosy_rinui::run(Box::new(gui_next::Model::new()?))?;
    if status != 0 {
        std::process::exit(status);
    }
    Ok(())
}
