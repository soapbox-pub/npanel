use super::db;
use super::types::{ClientType, DeviceType};
use indexmap::IndexMap;

// ---------------------------------------------------------------------------
// Internal data structs carried inside CompiledParser<T>
// ---------------------------------------------------------------------------

pub(crate) struct BotData {
    pub name: String,
    pub category: Option<String>,
    pub url: Option<String>,
    pub producer: Option<db::BotProducer>,
}

pub(crate) struct OsData {
    pub name: String,
    pub version_template: Option<String>,
    /// Version templates behind a further regex each, tried in order.
    pub versions: Vec<crate::parser::CompiledEntry<String>>,
}

pub(crate) struct ClientData {
    pub kind: ClientType,
    pub name: String,
    pub version_template: Option<String>,
    pub engine_default: Option<String>,
    pub engine_versions: Option<IndexMap<String, String>>,
}

pub(crate) struct EngineData {
    pub name: String,
}

pub(crate) struct DeviceBrandData {
    pub brand: String,
    pub model_template: Option<String>,
    pub device_type: Option<DeviceType>,
}

pub(crate) struct DeviceModelData {
    pub brand: Option<String>,
    pub model_template: Option<String>,
    pub device_type: Option<DeviceType>,
}

pub(crate) struct VendorFragmentData {
    pub brand: String,
}
