//! Italy — all of it (ADR 0018). The four Italy drivers behind the generic
//! traits: [`address`] (place-correlation bucket key), [`live_trains`]
//! (ViaggiaTreno client), [`rome`] (ATAC transit overlay), and [`netex`]
//! (NeTEx-IT profile). Each is selected from a registry arm in
//! [`crate::registry`]; the generic algorithms stay in their tiers.

pub(crate) mod address;
pub(crate) mod live_trains;
pub(crate) mod netex;
pub(crate) mod rome;
