//! Finds smart-home devices the way the SmartThings app does (OCF over CoAP, SSDP, mDNS,
//! BLE adverts and Wi-Fi SoftAP setup networks) and checks later whether they still answer.

pub mod ble;
pub mod cloud;
pub mod coap;
pub mod device;
pub mod matter;
pub mod mdns;
mod net;
pub mod ocf;
pub mod reach;
pub mod scan;
pub mod softap;
pub mod ssdp;
pub mod tv;

pub use device::{Device, Endpoint, Finding, Kind, Source};
