use crate::{
    compose::{load_compose_config, ComposeContext},
    status,
};
use anyhow::Result;

pub async fn show_status(context: &ComposeContext) -> Result<()> {
    let compose = load_compose_config(context, Some("*")).await?;
    let (services_info, status_map) =
        status::gather_status_data(context, &compose).await?;
    let formatted = status::format_status_table(&services_info, &status_map)?;
    print!("{}", formatted);
    Ok(())
}
