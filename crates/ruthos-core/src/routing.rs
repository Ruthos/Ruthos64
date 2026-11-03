//! Routing rules: send arriving images on to a PACS automatically
//! (Horos: auto-routing).
//!
//! A rule says which arrivals it applies to (received over the network, or
//! any import), optionally which modalities, and the PACS to send them to.
//! This module holds the rules and decides which images each one takes;
//! the interface batches them by study and sends them.

use serde::{Deserialize, Serialize};

/// Which arrivals a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteSource {
    /// The listener and PACS retrievals.
    Network,
    /// Also folders, files and pictures imported by hand.
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouteRule {
    pub enabled: bool,
    pub source: RouteSource,
    /// Modalities the rule takes, such as `US, CR`; empty for all.
    pub modalities: String,
    /// The destination PACS, by its name in Preferences > PACS.
    pub destination: String,
}

impl Default for RouteRule {
    fn default() -> Self {
        Self {
            enabled: true,
            source: RouteSource::Network,
            modalities: String::new(),
            destination: String::new(),
        }
    }
}

impl RouteRule {
    /// The modalities as a list, upper case.
    pub fn modality_list(&self) -> Vec<String> {
        self.modalities
            .split([',', ' ', ';', '\\'])
            .map(|m| m.trim().to_ascii_uppercase())
            .filter(|m| !m.is_empty())
            .collect()
    }

    /// Whether the rule takes an image of `modality` arriving this way.
    pub fn takes(&self, from_network: bool, modality: &str) -> bool {
        if !self.enabled || self.destination.trim().is_empty() {
            return false;
        }
        if self.source == RouteSource::Network && !from_network {
            return false;
        }
        let list = self.modality_list();
        list.is_empty() || list.iter().any(|m| m.eq_ignore_ascii_case(modality.trim()))
    }

    /// The rule in a sentence: "Send US and CR images received over the
    /// network to Clinic PACS."
    pub fn sentence(&self) -> String {
        let list = self.modality_list();
        let what = match list.len() {
            0 => "all images".to_string(),
            1 => format!("{} images", list[0]),
            _ => {
                let (last, rest) = list.split_last().unwrap();
                format!("{} and {last} images", rest.join(", "))
            }
        };
        let from = match self.source {
            RouteSource::Network => "received over the network",
            RouteSource::Any => "from any import",
        };
        let to = if self.destination.trim().is_empty() {
            "(choose a PACS)"
        } else {
            self.destination.trim()
        };
        format!("Send {what} {from} to {to}.")
    }
}

/// An imported image as routing sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteItem {
    pub image_id: i64,
    pub study_id: i64,
    pub study_uid: String,
    pub study_label: String,
    pub modality: String,
    /// As stored in the archive (`DataPaths::resolve` gives the file).
    pub path: String,
}

/// The images each rule takes from one import, by rule index. A study
/// retrieved from a PACS is never routed back to that same PACS.
pub fn assign(
    rules: &[RouteRule],
    from_network: bool,
    items: &[RouteItem],
    retrieved_from: &dyn Fn(&str) -> Option<String>,
) -> Vec<(usize, Vec<RouteItem>)> {
    let mut out = Vec::new();
    for (i, rule) in rules.iter().enumerate() {
        let taken: Vec<RouteItem> = items
            .iter()
            .filter(|it| rule.takes(from_network, &it.modality))
            .filter(|it| {
                retrieved_from(&it.study_uid).is_none_or(|src| !src.eq_ignore_ascii_case(rule.destination.trim()))
            })
            .cloned()
            .collect();
        if !taken.is_empty() {
            out.push((i, taken));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: i64, modality: &str, study: &str) -> RouteItem {
        RouteItem {
            image_id: id,
            study_id: 1,
            study_uid: study.into(),
            study_label: "REX".into(),
            modality: modality.into(),
            path: format!("{id}.dcm"),
        }
    }

    #[test]
    fn rules_match_source_and_modality() {
        let us = RouteRule {
            modalities: "us, cr".into(),
            destination: "Clinic PACS".into(),
            ..Default::default()
        };
        assert!(us.takes(true, "US"));
        assert!(us.takes(true, "CR"));
        assert!(!us.takes(true, "CT"));
        assert!(!us.takes(false, "US"), "a network rule ignores folder imports");
        let any = RouteRule {
            source: RouteSource::Any,
            destination: "Archive".into(),
            ..Default::default()
        };
        assert!(any.takes(false, "MR") && any.takes(true, ""));
        let off = RouteRule {
            enabled: false,
            ..any.clone()
        };
        assert!(!off.takes(true, "MR"));
        let nowhere = RouteRule::default();
        assert!(!nowhere.takes(true, "US"), "a rule without a destination does nothing");
        assert_eq!(
            us.sentence(),
            "Send US and CR images received over the network to Clinic PACS."
        );
        assert_eq!(any.sentence(), "Send all images from any import to Archive.");
    }

    #[test]
    fn a_retrieved_study_is_not_sent_back_where_it_came_from() {
        let rules = vec![
            RouteRule {
                destination: "PACS A".into(),
                ..Default::default()
            },
            RouteRule {
                destination: "PACS B".into(),
                ..Default::default()
            },
        ];
        let items = vec![item(1, "US", "1.2.3"), item(2, "US", "1.2.4")];
        let from = |uid: &str| (uid == "1.2.3").then(|| "pacs a".to_string());
        let got = assign(&rules, true, &items, &from);
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].1.iter().map(|i| i.image_id).collect::<Vec<_>>(),
            vec![2],
            "1.2.3 came from PACS A"
        );
        assert_eq!(got[1].1.len(), 2, "both go to PACS B");
    }

    #[test]
    fn rules_round_trip_through_settings_json() {
        let r = RouteRule {
            modalities: "DX".into(),
            destination: "X".into(),
            source: RouteSource::Any,
            enabled: false,
        };
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains("\"any\""));
        assert_eq!(serde_json::from_str::<RouteRule>(&text).unwrap(), r);
    }
}
