//! Quick parse types for normalized output of `docker compose config --profiles`

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct Compose {
    pub services: BTreeMap<String, Option<Service>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Service {
    pub image: Option<String>,
    pub labels: Option<BTreeMap<String, String>>,
    pub profiles: Option<Vec<String>>,
}
