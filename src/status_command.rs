use crate::{compose::ComposeContext, status};
use anyhow::Result;

pub async fn show_status(context: &ComposeContext) -> Result<()> {
    let (services_info, status_map) = status::gather_status_data(context).await?;
    let formatted = status::format_status_table(&services_info, &status_map)?;
    print!("{}", formatted);
    Ok(())
}
