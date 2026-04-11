use anyhow::Result;
use arboard::Clipboard;

pub fn read() -> Result<String> {
    Ok(Clipboard::new()?.get_text()?)
}

pub fn write(text: &str) -> Result<()> {
    Clipboard::new()?.set_text(text)?;
    Ok(())
}
