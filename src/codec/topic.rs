use crate::error::{Error, Result};

/// Validate the MQTT-specific shape of a Topic Name.
///
/// UTF-8, NUL, and encoded-length checks remain centralized in the string
/// encoder/decoder. Topic aliases are not supported, so an empty name is never
/// valid for this crate.
pub(crate) fn validate_topic_name(topic: &str) -> Result<()> {
    if topic.is_empty() {
        return Err(Error::MalformedPacket("topic name is empty"));
    }
    if topic.contains(['#', '+']) {
        return Err(Error::MalformedPacket("topic name contains a wildcard"));
    }
    Ok(())
}

/// Validate MQTT Topic Filter wildcard placement, including the standard
/// `$share/{ShareName}/{filter}` form.
///
/// UTF-8, NUL, and encoded-length checks remain centralized in the string
/// encoder.
pub(crate) fn validate_topic_filter(filter: &str) -> Result<()> {
    if filter.is_empty() {
        return Err(Error::MalformedPacket("topic filter is empty"));
    }

    if let Some(shared) = filter.strip_prefix("$share/") {
        let Some((share_name, inner_filter)) = shared.split_once('/') else {
            return Err(Error::MalformedPacket(
                "shared subscription is missing its topic filter",
            ));
        };
        if share_name.is_empty() || share_name.contains(['#', '+']) {
            return Err(Error::MalformedPacket(
                "shared subscription has an invalid share name",
            ));
        }
        if inner_filter.is_empty() {
            return Err(Error::MalformedPacket(
                "shared subscription is missing its topic filter",
            ));
        }
        return validate_filter_levels(inner_filter);
    }

    validate_filter_levels(filter)
}

fn validate_filter_levels(filter: &str) -> Result<()> {
    let mut levels = filter.split('/').peekable();
    while let Some(level) = levels.next() {
        if level.contains('#') && (level != "#" || levels.peek().is_some()) {
            return Err(Error::MalformedPacket(
                "multi-level wildcard must occupy the final topic level",
            ));
        }
        if level.contains('+') && level != "+" {
            return Err(Error::MalformedPacket(
                "single-level wildcard must occupy an entire topic level",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_topic_names_without_wildcards() {
        for topic in ["events", "events/new", "/", "$share/group/events"] {
            assert!(validate_topic_name(topic).is_ok(), "rejected {topic:?}");
        }
    }

    #[test]
    fn rejects_empty_or_wildcard_topic_names() {
        for topic in ["", "#", "+", "events/#", "events/+/new"] {
            assert!(validate_topic_name(topic).is_err(), "accepted {topic:?}");
        }
    }

    #[test]
    fn accepts_standard_and_shared_topic_filters() {
        for filter in [
            "events",
            "events/#",
            "events/+/new",
            "+",
            "#",
            "/",
            "/+",
            "+/",
            "$share/workers/events/#",
            "$share/workers/+/new",
        ] {
            assert!(validate_topic_filter(filter).is_ok(), "rejected {filter:?}");
        }
    }

    #[test]
    fn rejects_empty_malformed_or_invalid_shared_topic_filters() {
        for filter in [
            "",
            "events#",
            "events/#/new",
            "events/+new",
            "events/new+",
            "##",
            "$share/workers",
            "$share/workers/",
            "$share//events",
            "$share/+/events",
            "$share/#/events",
        ] {
            assert!(
                validate_topic_filter(filter).is_err(),
                "accepted {filter:?}"
            );
        }
    }
}
