//! Which of the playlist, the setlist and the inspector show for a given window width.
//!
//! What the user asked for is kept apart from what fits, so a window that grows again brings back
//! exactly what was open. When room runs out the sections give way in steps: three columns, then the
//! playlist beside one column that holds the setlist and the inspector as tabs, then one section alone.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Playlist,
    Setlist,
    Inspector,
}

/// The sections the user has open, whether or not they fit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wanted {
    pub playlist: bool,
    pub setlist: bool,
    pub inspector: bool,
}

impl Wanted {
    fn get(self, s: Section) -> bool {
        match s {
            Section::Playlist => self.playlist,
            Section::Setlist => self.setlist,
            Section::Inspector => self.inspector,
        }
    }
}

/// The widths the layout works with.
#[derive(Clone, Copy, Debug)]
pub struct Widths {
    pub total: f32,
    /// The narrowest the playlist can be before its toolbar breaks.
    pub playlist_min: f32,
    /// The narrowest a setlist or inspector column can be.
    pub side_min: f32,
}

/// What is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub playlist: bool,
    pub setlist: bool,
    pub inspector: bool,
    /// The setlist and the inspector are both open but share one column, which draws only one of them.
    pub tabs: bool,
}

impl Plan {
    pub fn shows(self, s: Section) -> bool {
        match s {
            Section::Playlist => self.playlist,
            Section::Setlist => self.setlist,
            Section::Inspector => self.inspector,
        }
    }

    pub fn any(self) -> bool {
        self.playlist || self.setlist || self.inspector
    }
}

/// Fits `want` into `w`. `recent` is the section the user opened last: it wins the room when only one fits
/// and picks the tab when two share a column. Beyond that the playlist ranks first, then the setlist.
pub fn plan(want: Wanted, w: Widths, recent: Section) -> Plan {
    let side = [Section::Setlist, Section::Inspector].into_iter().filter(|&s| want.get(s)).count();
    let room = if want.playlist {
        ((w.total - w.playlist_min) / w.side_min).floor().max(0.0) as usize
    } else {
        ((w.total / w.side_min).floor() as usize).max(1)
    };
    let only = |s: Section| Plan { playlist: s == Section::Playlist, setlist: s == Section::Setlist, inspector: s == Section::Inspector, tabs: false };
    if room >= side {
        return Plan { playlist: want.playlist, setlist: want.setlist, inspector: want.inspector, tabs: false };
    }
    if room == 0 {
        return only(if want.get(recent) { recent } else { Section::Playlist });
    }
    // One column for two sections.
    let tab = if recent == Section::Inspector { Section::Inspector } else { Section::Setlist };
    Plan { playlist: want.playlist, setlist: tab == Section::Setlist, inspector: tab == Section::Inspector, tabs: true }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(non_snake_case)]
    fn W(total: f32) -> Widths {
        Widths { total, playlist_min: 400.0, side_min: 300.0 }
    }
    const ALL: Wanted = Wanted { playlist: true, setlist: true, inspector: true };

    #[test]
    fn wide_window_shows_all_three() {
        let p = plan(ALL, W(1100.0), Section::Playlist);
        assert_eq!(p, Plan { playlist: true, setlist: true, inspector: true, tabs: false });
    }

    #[test]
    fn medium_window_shares_one_column_between_setlist_and_inspector() {
        let p = plan(ALL, W(800.0), Section::Playlist);
        assert!(p.playlist && p.tabs && p.setlist && !p.inspector, "the setlist is the default tab");
        let p = plan(ALL, W(800.0), Section::Inspector);
        assert!(p.playlist && p.tabs && !p.setlist && p.inspector, "the section opened last takes the tab");
    }

    #[test]
    fn narrow_window_shows_the_section_opened_last_or_the_playlist() {
        assert_eq!(plan(ALL, W(600.0), Section::Playlist), Plan { playlist: true, ..Plan::default() });
        assert_eq!(plan(ALL, W(600.0), Section::Inspector), Plan { inspector: true, ..Plan::default() });
        let no_setlist = Wanted { setlist: false, ..ALL };
        assert_eq!(plan(no_setlist, W(600.0), Section::Setlist), Plan { playlist: true, ..Plan::default() }, "a section that is not open cannot win");
    }

    #[test]
    fn one_side_section_gets_its_column_when_there_is_room_for_one() {
        let want = Wanted { setlist: false, ..ALL };
        let p = plan(want, W(800.0), Section::Playlist);
        assert_eq!(p, Plan { playlist: true, inspector: true, ..Plan::default() });
        assert_eq!(plan(want, W(600.0), Section::Inspector), Plan { inspector: true, ..Plan::default() });
    }

    #[test]
    fn without_the_playlist_the_side_sections_share_the_width() {
        let want = Wanted { playlist: false, ..ALL };
        assert_eq!(plan(want, W(640.0), Section::Playlist), Plan { setlist: true, inspector: true, ..Plan::default() });
        assert!(!plan(Wanted::default(), W(640.0), Section::Playlist).any());
    }

    #[test]
    fn nothing_is_lost_by_narrowing() {
        let narrow = plan(ALL, W(500.0), Section::Playlist);
        assert_eq!(narrow, Plan { playlist: true, ..Plan::default() });
        assert_eq!(plan(ALL, W(1200.0), Section::Playlist), Plan { playlist: true, setlist: true, inspector: true, tabs: false });
    }
}
