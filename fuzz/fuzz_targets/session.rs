#![no_main]
//! Session and recovery files are user data that may be truncated or
//! corrupted; parsing must never panic and valid sessions must round-trip.
use apex_project::session::Session;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = serde_json::from_slice::<Session>(data) {
        let again = serde_json::to_vec(&s).expect("serialise");
        let back: Session = serde_json::from_slice(&again).expect("round trip");
        assert_eq!(serde_json::to_vec(&back).unwrap(), again);
    }
    let _ = serde_json::from_slice::<apex_project::session::RecoveryEntry>(data);
});
