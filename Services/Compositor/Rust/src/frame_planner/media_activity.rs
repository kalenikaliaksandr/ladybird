/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use libcompositing_rust::display_list::summary::LiveContent;
use libcompositing_rust::time::{Duration, MonotonicTime};

pub const LONGEST_TIME_BETWEEN_UPDATES_THAT_ACTIVATE_MEDIA: Duration = Duration::from_milliseconds(1000);
pub const TIME_MEDIA_STAYS_ACTIVE_AFTER_AN_UPDATE: Duration = Duration::from_milliseconds(3000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaUpdate {
    pub media: LiveContent,
    pub content_version: u64,
}

#[derive(Clone, Copy, Debug)]
struct MediaUpdates {
    media: LiveContent,
    content_version: u64,
    last_update: MonotonicTime,
    active: bool,
}

#[derive(Default)]
pub struct MediaActivity {
    updates: Vec<MediaUpdates>,
}

impl MediaActivity {
    pub fn note_updates(&mut self, updates: &[MediaUpdate], now: MonotonicTime) {
        self.updates
            .retain(|media_updates| now - media_updates.last_update <= TIME_MEDIA_STAYS_ACTIVE_AFTER_AN_UPDATE);
        for update in updates {
            self.note_update(update, now);
        }
    }

    fn note_update(&mut self, update: &MediaUpdate, now: MonotonicTime) {
        if let Some(updates) = self.updates.iter_mut().find(|updates| updates.media == update.media) {
            if updates.content_version == update.content_version {
                return;
            }
            if now - updates.last_update <= LONGEST_TIME_BETWEEN_UPDATES_THAT_ACTIVATE_MEDIA {
                updates.active = true;
            }
            updates.content_version = update.content_version;
            updates.last_update = now;
            return;
        }
        self.updates.push(MediaUpdates {
            media: update.media,
            content_version: update.content_version,
            last_update: now,
            active: false,
        });
    }

    pub fn active_media(&self, now: MonotonicTime) -> Vec<LiveContent> {
        self.updates
            .iter()
            .filter(|updates| updates.active && now - updates.last_update <= TIME_MEDIA_STAYS_ACTIVE_AFTER_AN_UPDATE)
            .map(|updates| updates.media)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libcompositing_rust::display_list::commands::CanvasId;

    const CANVAS: LiveContent = LiveContent::Canvas(CanvasId(1));

    fn at_milliseconds(milliseconds: i64) -> MonotonicTime {
        MonotonicTime::from_nanoseconds(Duration::from_milliseconds(milliseconds).nanoseconds())
    }

    fn update(content_version: u64) -> MediaUpdate {
        MediaUpdate {
            media: CANVAS,
            content_version,
        }
    }

    #[test]
    fn a_second_update_within_a_second_activates_media() {
        let mut activity = MediaActivity::default();
        activity.note_updates(&[update(1)], at_milliseconds(0));
        assert!(activity.active_media(at_milliseconds(0)).is_empty());
        activity.note_updates(&[update(2)], at_milliseconds(900));
        assert_eq!(activity.active_media(at_milliseconds(900)), [CANVAS]);
    }

    #[test]
    fn updates_further_apart_than_a_second_leave_media_inactive() {
        let mut activity = MediaActivity::default();
        activity.note_updates(&[update(1)], at_milliseconds(0));
        activity.note_updates(&[update(2)], at_milliseconds(1500));
        assert!(activity.active_media(at_milliseconds(1500)).is_empty());
    }

    #[test]
    fn an_update_with_the_content_version_already_noted_is_no_update() {
        let mut activity = MediaActivity::default();
        activity.note_updates(&[update(1)], at_milliseconds(0));
        activity.note_updates(&[update(1)], at_milliseconds(100));
        assert!(activity.active_media(at_milliseconds(100)).is_empty());
    }

    #[test]
    fn media_stays_active_for_three_seconds_after_its_last_update() {
        let mut activity = MediaActivity::default();
        activity.note_updates(&[update(1)], at_milliseconds(0));
        activity.note_updates(&[update(2)], at_milliseconds(100));
        assert_eq!(activity.active_media(at_milliseconds(3100)), [CANVAS]);
        assert!(activity.active_media(at_milliseconds(3101)).is_empty());
        activity.note_updates(&[], at_milliseconds(3101));
        activity.note_updates(&[update(3)], at_milliseconds(3200));
        assert!(activity.active_media(at_milliseconds(3200)).is_empty());
    }
}
