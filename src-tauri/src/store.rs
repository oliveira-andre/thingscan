//! The devices the user added, kept in `devices.json` in the app data folder.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thingscan_core::cloud::CloudDevice;
use thingscan_core::reach::{Report, Status};
use thingscan_core::{Device, Endpoint, Kind, Source};

const FILE: &str = "devices.json";
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDevice {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub room: Option<String>,
    pub kind: Kind,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Stable identities (`ocf:<di>`, `upnp:<udn>`, `ble:<id>`, …) used to recognise it in a scan.
    pub ids: Vec<String>,
    /// Where to check it; replaced when it is found again under a new address.
    pub endpoints: Vec<Endpoint>,
    #[serde(default)]
    pub sources: BTreeSet<Source>,
    /// What the scan said about it when it was added.
    #[serde(default)]
    pub details: BTreeMap<String, String>,
    /// Seconds since the epoch.
    pub added_at: u64,
    #[serde(default)]
    pub last_seen: Option<u64>,
    #[serde(default)]
    pub last_address: Option<String>,
    /// The matching device in the user's SmartThings account, used for controls.
    #[serde(default)]
    pub cloud_id: Option<String>,
    /// The token a Samsung TV issued when its remote was allowed. Kept so the TV doesn't ask again.
    #[serde(default)]
    pub tv_token: Option<String>,
    /// When the user last changed it (name, room, SmartThings link, TV pairing), in seconds.
    /// Account sync keeps the newest edit; checks and moves don't count.
    #[serde(default)]
    pub edited_at: u64,
}

/// A device removed from the home, remembered so account sync doesn't bring it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    pub id: String,
    pub removed_at: u64,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    devices: Vec<SavedDevice>,
    #[serde(default)]
    removed: Vec<Removed>,
}

pub struct Store {
    path: PathBuf,
    devices: Mutex<Vec<SavedDevice>>,
    removed: Mutex<Vec<Removed>>,
    /// Counts writes, so account sync notices changes.
    generation: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum StoreError {
    NotFound(String),
    Io(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound(m) | StoreError::Io(m) => f.write_str(m),
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn clean(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

impl Store {
    /// Opens the store in `dir`. A missing file is an empty home; an unreadable one is an
    /// error rather than being overwritten.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(dir).map_err(|e| StoreError::Io(format!("can't create {}: {e}", dir.display())))?;
        let path = dir.join(FILE);
        let (mut devices, removed) = match fs::read(&path) {
            Ok(bytes) => {
                let file = serde_json::from_slice::<FileFormat>(&bytes)
                    .map_err(|e| StoreError::Io(format!("can't read {}: {e}", path.display())))?;
                (file.devices, file.removed)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
            Err(e) => return Err(StoreError::Io(format!("can't read {}: {e}", path.display()))),
        };
        // Saved before edits were timed: count adding it as the last edit.
        for device in devices.iter_mut().filter(|d| d.edited_at == 0) {
            device.edited_at = device.added_at;
        }
        Ok(Self { path, devices: Mutex::new(devices), removed: Mutex::new(removed), generation: AtomicU64::new(0) })
    }

    pub fn list(&self) -> Vec<SavedDevice> {
        self.devices.lock().unwrap().clone()
    }

    /// The devices and the removals, for account sync, with the generation they were read at.
    pub fn snapshot(&self) -> (u64, Vec<SavedDevice>, Vec<Removed>) {
        let devices = self.devices.lock().unwrap();
        let removed = self.removed.lock().unwrap();
        (self.generation.load(Ordering::SeqCst), devices.clone(), removed.clone())
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Replaces everything with what account sync merged, unless something was saved since
    /// `generation` (then the caller merges again). Returns whether anything changed.
    pub fn replace(
        &self,
        generation: u64,
        new_devices: Vec<SavedDevice>,
        new_removed: Vec<Removed>,
    ) -> Result<Option<bool>, StoreError> {
        let mut devices = self.devices.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(None);
        }
        if *devices == new_devices && *self.removed.lock().unwrap() == new_removed {
            return Ok(Some(false));
        }
        *devices = new_devices;
        *self.removed.lock().unwrap() = new_removed;
        self.write(&devices)?;
        Ok(Some(true))
    }

    /// Adds a scanned device. If it is already in the home (any shared id), that entry is
    /// refreshed with the new endpoints and name instead of adding a duplicate.
    pub fn add(&self, device: Device, name: String, room: Option<String>) -> Result<SavedDevice, StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let name = clean(Some(name)).unwrap_or_else(|| device.name.clone());
        let room = clean(room);
        let saved = match devices.iter_mut().find(|d| device.shares_identity(&d.ids)) {
            Some(existing) => {
                existing.name = name;
                existing.room = room;
                for id in &device.ids {
                    if !existing.ids.contains(id) {
                        existing.ids.push(id.clone());
                    }
                }
                existing.endpoints = device.endpoints;
                existing.last_address = Some(device.address);
                existing.edited_at = now();
                existing.clone()
            }
            None => {
                let saved = SavedDevice {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    room,
                    kind: device.kind,
                    manufacturer: device.manufacturer,
                    model: device.model,
                    ids: device.ids,
                    endpoints: device.endpoints,
                    sources: device.sources,
                    details: device.details,
                    added_at: now(),
                    last_seen: Some(now()),
                    last_address: Some(device.address),
                    cloud_id: None,
                    tv_token: None,
                    edited_at: now(),
                };
                devices.push(saved.clone());
                saved
            }
        };
        self.write(&devices)?;
        Ok(saved)
    }

    pub fn update(&self, id: &str, name: String, room: Option<String>) -> Result<SavedDevice, StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices.iter_mut().find(|d| d.id == id).ok_or_else(|| not_found(id))?;
        if let Some(name) = clean(Some(name)) {
            device.name = name;
        }
        device.room = clean(room);
        device.edited_at = now();
        let saved = device.clone();
        self.write(&devices)?;
        Ok(saved)
    }

    /// Adds a device from the SmartThings account, or links it to the saved device it already is.
    pub fn add_cloud(
        &self,
        cloud: &CloudDevice,
        name: String,
        room: Option<String>,
    ) -> Result<SavedDevice, StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let ids = cloud_ids(cloud);
        let saved = match devices.iter_mut().find(|d| d.ids.iter().any(|id| ids.contains(id))) {
            Some(existing) => {
                existing.name = clean(Some(name)).unwrap_or_else(|| existing.name.clone());
                existing.room = clean(room).or(existing.room.take());
                attach_cloud(existing, cloud);
                existing.edited_at = now();
                existing.clone()
            }
            None => {
                let mut saved = SavedDevice {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: clean(Some(name)).unwrap_or_else(|| cloud.name.clone()),
                    room: clean(room),
                    kind: kind_of_category(cloud.category.as_deref()),
                    manufacturer: cloud.manufacturer.clone(),
                    model: cloud.model.clone(),
                    ids: Vec::new(),
                    endpoints: Vec::new(),
                    sources: BTreeSet::new(),
                    details: BTreeMap::new(),
                    added_at: now(),
                    last_seen: None,
                    last_address: None,
                    cloud_id: None,
                    tv_token: None,
                    edited_at: now(),
                };
                if let Some(category) = &cloud.category {
                    saved.details.insert("cloud.category".into(), category.clone());
                }
                attach_cloud(&mut saved, cloud);
                devices.push(saved.clone());
                saved
            }
        };
        self.write(&devices)?;
        Ok(saved)
    }

    /// Links a saved device to a SmartThings device, or unlinks it with `None`.
    pub fn link_cloud(&self, id: &str, cloud: Option<&CloudDevice>) -> Result<SavedDevice, StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices.iter_mut().find(|d| d.id == id).ok_or_else(|| not_found(id))?;
        match cloud {
            Some(cloud) => attach_cloud(device, cloud),
            None => detach_cloud(device),
        }
        device.edited_at = now();
        let saved = device.clone();
        self.write(&devices)?;
        Ok(saved)
    }

    /// Links every unlinked saved device to the SmartThings device with the same OCF id or,
    /// failing that, the only one of its exact model (a TV found over UPnP has no OCF id).
    /// Returns the devices that changed.
    pub fn auto_link(&self, cloud: &[CloudDevice]) -> Result<Vec<SavedDevice>, StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let mut changed = Vec::new();
        for index in 0..devices.len() {
            if devices[index].cloud_id.is_some() {
                continue;
            }
            let found = cloud
                .iter()
                .find(|c| cloud_ids(c).iter().any(|id| devices[index].ids.contains(id)))
                .or_else(|| only_one_of_model(&devices, index, cloud));
            if let Some(found) = found {
                attach_cloud(&mut devices[index], found);
                devices[index].edited_at = now();
                changed.push(devices[index].clone());
            }
        }
        if !changed.is_empty() {
            self.write(&devices)?;
        }
        Ok(changed)
    }

    /// Unlinks every device, when the SmartThings account is disconnected.
    pub fn unlink_all(&self) -> Result<(), StoreError> {
        let mut devices = self.devices.lock().unwrap();
        for device in devices.iter_mut().filter(|d| d.cloud_id.is_some()) {
            detach_cloud(device);
            device.edited_at = now();
        }
        self.write(&devices)
    }

    pub fn get(&self, id: &str) -> Result<SavedDevice, StoreError> {
        self.devices.lock().unwrap().iter().find(|d| d.id == id).cloned().ok_or_else(|| not_found(id))
    }

    /// Remembers a TV's MAC address for Wake-on-LAN.
    pub fn set_tv_mac(&self, id: &str, mac: String) -> Result<(), StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices.iter_mut().find(|d| d.id == id).ok_or_else(|| not_found(id))?;
        for endpoint in &mut device.endpoints {
            if let Endpoint::SamsungTv { mac: slot, .. } = endpoint {
                *slot = Some(mac.clone());
            }
        }
        device.details.insert("tv.wifi_mac".into(), mac);
        self.write(&devices)
    }

    pub fn set_tv_token(&self, id: &str, token: String) -> Result<(), StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices.iter_mut().find(|d| d.id == id).ok_or_else(|| not_found(id))?;
        device.tv_token = Some(token);
        self.write(&devices)
    }

    /// Moves every device in room `from` to `to`, or out of any room with `None`. Returns the
    /// devices that moved.
    pub fn rename_room(&self, from: &str, to: Option<String>) -> Result<Vec<SavedDevice>, StoreError> {
        let to = clean(to);
        let mut devices = self.devices.lock().unwrap();
        let mut moved = Vec::new();
        for device in devices.iter_mut().filter(|d| d.room.as_deref() == Some(from)) {
            device.room = to.clone();
            device.edited_at = now();
            moved.push(device.clone());
        }
        if moved.is_empty() {
            return Err(StoreError::NotFound(format!("no room called {from}")));
        }
        self.write(&devices)?;
        Ok(moved)
    }

    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let before = devices.len();
        devices.retain(|d| d.id != id);
        if devices.len() == before {
            return Err(not_found(id));
        }
        self.removed.lock().unwrap().push(Removed { id: id.to_string(), removed_at: now() });
        self.write(&devices)
    }

    /// Records a reachability result: when it was last seen, and its new endpoints if it moved.
    pub fn record(&self, report: &Report) -> Result<Option<SavedDevice>, StoreError> {
        let mut devices = self.devices.lock().unwrap();
        let Some(device) = devices.iter_mut().find(|d| d.id == report.id) else { return Ok(None) };
        if report.status == Status::Reachable {
            device.last_seen = Some(now());
            device.last_address = report.address.clone().or(device.last_address.take());
        }
        if let Some(endpoints) = &report.relocated {
            device.endpoints = endpoints.clone();
        }
        let saved = device.clone();
        self.write(&devices)?;
        Ok(Some(saved))
    }

    /// Write to a temporary file and rename it into place, so a crash never leaves half a file.
    fn write(&self, devices: &[SavedDevice]) -> Result<(), StoreError> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let removed = self.removed.lock().unwrap().clone();
        let file = FileFormat { version: VERSION, devices: devices.to_vec(), removed };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| StoreError::Io(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| StoreError::Io(format!("can't write {}: {e}", tmp.display())))?;
        fs::rename(&tmp, &self.path).map_err(|e| StoreError::Io(format!("can't save {}: {e}", self.path.display())))
    }
}

/// The SmartThings device of the same model as `devices[index]`, when neither side has another
/// unlinked device of that model to confuse it with.
fn only_one_of_model<'a>(devices: &[SavedDevice], index: usize, cloud: &'a [CloudDevice]) -> Option<&'a CloudDevice> {
    let model = devices[index].model.as_deref().filter(|m| !m.is_empty())?;
    let same = |m: &Option<String>| m.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(model));
    let linked: BTreeSet<&str> = devices.iter().filter_map(|d| d.cloud_id.as_deref()).collect();
    let mut candidates = cloud.iter().filter(|c| same(&c.model) && !linked.contains(c.device_id.as_str()));
    let found = candidates.next()?;
    let rivals = devices.iter().filter(|d| d.cloud_id.is_none() && same(&d.model)).count();
    (candidates.next().is_none() && rivals == 1).then_some(found)
}

/// The ids a SmartThings device is known by: its cloud id, and its OCF id when it has one.
fn cloud_ids(cloud: &CloudDevice) -> Vec<String> {
    let mut ids = vec![format!("st:{}", cloud.device_id)];
    if let Some(di) = &cloud.di {
        ids.push(format!("ocf:{di}"));
    }
    ids
}

fn attach_cloud(device: &mut SavedDevice, cloud: &CloudDevice) {
    detach_cloud(device);
    device.cloud_id = Some(cloud.device_id.clone());
    for id in cloud_ids(cloud) {
        if !device.ids.contains(&id) {
            device.ids.push(id);
        }
    }
    device.endpoints.push(Endpoint::Cloud { device_id: cloud.device_id.clone() });
    if device.manufacturer.is_none() {
        device.manufacturer = cloud.manufacturer.clone();
    }
    if device.model.is_none() {
        device.model = cloud.model.clone();
    }
}

pub(crate) fn detach_cloud(device: &mut SavedDevice) {
    if let Some(old) = device.cloud_id.take() {
        device.ids.retain(|id| *id != format!("st:{old}"));
    }
    device.endpoints.retain(|e| !matches!(e, Endpoint::Cloud { .. }));
}

/// The app's device kind for a SmartThings category.
pub fn kind_of_category(category: Option<&str>) -> Kind {
    match category.unwrap_or_default() {
        "Television" | "TV" => Kind::Tv,
        "Hub" | "Bridges" => Kind::Hub,
        "AirConditioner"
        | "Washer"
        | "Dryer"
        | "Refrigerator"
        | "Dishwasher"
        | "Oven"
        | "Range"
        | "Microwave"
        | "RobotCleaner"
        | "AirPurifier"
        | "Humidifier"
        | "Dehumidifier"
        | "Cooktop"
        | "KimchiRefrigerator"
        | "WaterHeater"
        | "ClothingCareMachine" => Kind::Appliance,
        _ => Kind::Other,
    }
}

fn not_found(id: &str) -> StoreError {
    StoreError::NotFound(format!("no device with id {id}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use thingscan_core::reach::Probe;

    pub(crate) fn scanned(ids: &[&str], ip: &str) -> Device {
        Device {
            key: ids[0].to_string(),
            kind: Kind::Appliance,
            name: "Samsung-Room-Air-Conditioner".into(),
            address: format!("{ip}:49153"),
            sources: BTreeSet::from([Source::Ocf]),
            manufacturer: Some("Samsung Electronics".into()),
            model: Some("ARA-WW-TP1-22-COMMON".into()),
            ids: ids.iter().map(|s| s.to_string()).collect(),
            endpoints: vec![Endpoint::Ocf { ip: ip.parse().unwrap(), di: Some("d1".into()) }],
            details: BTreeMap::new(),
        }
    }

    #[test]
    fn adds_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let saved =
            store.add(scanned(&["ocf:d1"], "192.168.3.7"), "  Bedroom A/C ".into(), Some("Bedroom".into())).unwrap();
        assert_eq!(saved.name, "Bedroom A/C");
        assert_eq!(saved.room.as_deref(), Some("Bedroom"));

        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].id, saved.id);
    }

    #[test]
    fn re_adding_the_same_device_updates_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let first = store.add(scanned(&["ocf:d1"], "192.168.3.7"), "A/C".into(), None).unwrap();
        let again = store.add(scanned(&["ocf:d1", "ble:p1"], "192.168.3.9"), "Office A/C".into(), None).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(again.id, first.id);
        assert_eq!(again.name, "Office A/C");
        assert_eq!(again.ids, vec!["ocf:d1".to_string(), "ble:p1".to_string()]);
        assert_eq!(again.last_address.as_deref(), Some("192.168.3.9:49153"));
    }

    #[test]
    fn records_reachability_and_relocation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let saved = store.add(scanned(&["ocf:d1"], "192.168.3.7"), "A/C".into(), None).unwrap();
        let moved = vec![Endpoint::Ocf { ip: "192.168.3.9".parse().unwrap(), di: Some("d1".into()) }];
        let report = Report {
            id: saved.id.clone(),
            status: Status::Reachable,
            address: Some("192.168.3.9:49153".into()),
            probes: vec![Probe {
                via: "OCF".into(),
                target: "192.168.3.9".into(),
                status: Status::Reachable,
                detail: String::new(),
                latency_ms: Some(4),
            }],
            relocated: Some(moved.clone()),
        };
        let updated = store.record(&report).unwrap().unwrap();
        assert_eq!(updated.endpoints, moved);
        assert_eq!(Store::open(dir.path()).unwrap().list()[0].endpoints, moved);
    }

    #[test]
    fn rename_remove_and_missing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let saved = store.add(scanned(&["ocf:d1"], "192.168.3.7"), "A/C".into(), Some("Bedroom".into())).unwrap();
        let renamed = store.update(&saved.id, "Guest A/C".into(), Some("  ".into())).unwrap();
        assert_eq!(renamed.name, "Guest A/C");
        assert_eq!(renamed.room, None);
        store.remove(&saved.id).unwrap();
        assert!(store.list().is_empty());
        assert!(matches!(store.remove(&saved.id), Err(StoreError::NotFound(_))));
    }

    fn cloud_ac(di: &str) -> CloudDevice {
        CloudDevice {
            device_id: "c0ffee00-0000-0000-0000-000000000001".into(),
            name: "Bedroom A/C".into(),
            manufacturer: Some("Samsung Electronics".into()),
            category: Some("AirConditioner".into()),
            ocf_type: Some("oic.d.airconditioner".into()),
            model: Some("ARA-WW-TP1-22-COMMON".into()),
            capabilities: vec!["switch".into(), "airConditionerMode".into()],
            di: Some(di.into()),
        }
    }

    #[test]
    fn links_scanned_devices_to_smartthings_by_ocf_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let saved = store.add(scanned(&["ocf:d1"], "192.168.3.7"), "A/C".into(), None).unwrap();
        let changed = store.auto_link(&[cloud_ac("other"), cloud_ac("d1")]).unwrap();
        assert_eq!(changed.len(), 1);
        let linked = store.get(&saved.id).unwrap();
        assert_eq!(linked.cloud_id.as_deref(), Some("c0ffee00-0000-0000-0000-000000000001"));
        assert!(
            linked.endpoints.contains(&Endpoint::Cloud { device_id: "c0ffee00-0000-0000-0000-000000000001".into() })
        );
        // Already linked: nothing more to do.
        assert!(store.auto_link(&[cloud_ac("d1")]).unwrap().is_empty());

        store.unlink_all().unwrap();
        let unlinked = store.get(&saved.id).unwrap();
        assert_eq!(unlinked.cloud_id, None);
        assert!(!unlinked.endpoints.iter().any(|e| matches!(e, Endpoint::Cloud { .. })));
        assert!(!unlinked.ids.iter().any(|id| id.starts_with("st:")));
    }

    #[test]
    fn links_a_device_without_an_ocf_id_by_its_model_when_unambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut tv = scanned(&["upnp:uuid:tv"], "192.168.3.16");
        tv.kind = Kind::Tv;
        tv.model = Some("QN55LS03FAGXZD".into());
        let tv = store.add(tv, "TV".into(), None).unwrap();
        let ac = store.add(scanned(&["ocf:d1"], "192.168.3.4"), "A/C".into(), None).unwrap();
        let cloud_tv = CloudDevice {
            device_id: "t1".into(),
            name: "TV".into(),
            manufacturer: Some("Samsung Electronics".into()),
            category: Some("Television".into()),
            ocf_type: Some("oic.d.tv".into()),
            model: Some("qn55ls03fagxzd".into()),
            capabilities: vec!["switch".into()],
            di: Some("tv-di".into()),
        };
        // Two A/Cs of that model in the account: which one the saved A/C is can't be told.
        let mut other_ac = cloud_ac("d9");
        other_ac.device_id = "a2".into();
        let changed = store.auto_link(&[cloud_tv.clone(), cloud_ac("d8"), other_ac]).unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(store.get(&tv.id).unwrap().cloud_id.as_deref(), Some("t1"));
        assert_eq!(store.get(&ac.id).unwrap().cloud_id, None);

        // Nor when two saved devices share the model.
        store.add(scanned(&["ocf:d2"], "192.168.3.7"), "Other A/C".into(), None).unwrap();
        assert!(store.auto_link(&[cloud_ac("d8")]).unwrap().is_empty());
    }

    #[test]
    fn adds_cloud_only_devices_and_merges_known_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let washer = CloudDevice {
            device_id: "w1".into(),
            name: "Washer".into(),
            manufacturer: Some("Samsung Electronics".into()),
            category: Some("Washer".into()),
            ocf_type: Some("oic.d.washer".into()),
            model: None,
            capabilities: vec!["washerOperatingState".into()],
            di: None,
        };
        let added = store.add_cloud(&washer, "".into(), Some("Laundry".into())).unwrap();
        assert_eq!(added.name, "Washer");
        assert_eq!(added.kind, Kind::Appliance);
        assert_eq!(added.cloud_id.as_deref(), Some("w1"));

        let local = store.add(scanned(&["ocf:d1"], "192.168.3.7"), "A/C".into(), None).unwrap();
        let merged = store.add_cloud(&cloud_ac("d1"), "Bedroom A/C".into(), None).unwrap();
        assert_eq!(merged.id, local.id);
        assert_eq!(store.list().len(), 2);
    }

    #[test]
    fn renames_and_clears_rooms() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.add(scanned(&["ocf:a"], "192.168.3.4"), "A".into(), Some("Bedroom".into())).unwrap();
        store.add(scanned(&["ocf:b"], "192.168.3.7"), "B".into(), Some("Bedroom".into())).unwrap();
        store.add(scanned(&["ocf:c"], "192.168.3.9"), "C".into(), Some("Office".into())).unwrap();

        let moved = store.rename_room("Bedroom", Some(" Main bedroom ".into())).unwrap();
        assert_eq!(moved.len(), 2);
        let rooms: Vec<_> = Store::open(dir.path()).unwrap().list().into_iter().map(|d| d.room).collect();
        assert_eq!(rooms, vec![Some("Main bedroom".into()), Some("Main bedroom".into()), Some("Office".into())]);

        store.rename_room("Office", None).unwrap();
        assert_eq!(store.list()[2].room, None);
        assert!(matches!(store.rename_room("Garage", Some("Shed".into())), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn keeps_the_tv_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let saved = store.add(scanned(&["ocf:tv"], "192.168.3.16"), "TV".into(), None).unwrap();
        store.set_tv_token(&saved.id, "12345678".into()).unwrap();
        assert_eq!(Store::open(dir.path()).unwrap().get(&saved.id).unwrap().tv_token.as_deref(), Some("12345678"));
    }

    #[test]
    fn refuses_to_clobber_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), b"{ not json").unwrap();
        assert!(matches!(Store::open(dir.path()), Err(StoreError::Io(_))));
    }
}
