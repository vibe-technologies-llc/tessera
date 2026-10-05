use super::{EditError, Marker, MarkerId, Project};
use crate::time::Time;

impl Project {
    pub fn marker(&self, id: MarkerId) -> Option<&Marker> {
        self.markers.iter().find(|marker| marker.id == id)
    }

    pub fn add_marker(&mut self, time: Time, name: impl Into<String>) -> MarkerId {
        let id = self.next_ids.take_marker();
        self.insert_marker(Marker {
            id,
            time: time.max(Time::ZERO),
            name: name.into(),
        });
        id
    }

    pub fn move_marker(&mut self, id: MarkerId, time: Time) -> Result<(), EditError> {
        let marker = self.remove_marker(id)?;
        self.insert_marker(Marker {
            time: time.max(Time::ZERO),
            ..marker
        });
        Ok(())
    }

    pub fn rename_marker(
        &mut self,
        id: MarkerId,
        name: impl Into<String>,
    ) -> Result<(), EditError> {
        let marker = self
            .markers
            .iter_mut()
            .find(|marker| marker.id == id)
            .ok_or(EditError::UnknownMarker(id))?;
        marker.name = name.into();
        Ok(())
    }

    pub fn remove_marker(&mut self, id: MarkerId) -> Result<Marker, EditError> {
        let index = self
            .markers
            .iter()
            .position(|marker| marker.id == id)
            .ok_or(EditError::UnknownMarker(id))?;
        Ok(self.markers.remove(index))
    }

    pub fn next_marker_after(&self, time: Time) -> Option<&Marker> {
        let index = self.markers.partition_point(|marker| marker.time <= time);
        self.markers.get(index)
    }

    pub fn previous_marker_before(&self, time: Time) -> Option<&Marker> {
        let index = self.markers.partition_point(|marker| marker.time < time);
        self.markers.get(index.checked_sub(1)?)
    }

    pub fn set_in_point(&mut self, time: Time) -> Result<(), EditError> {
        let time = time.max(Time::ZERO);
        if self.out_point.is_some_and(|out| time >= out) {
            return Err(EditError::InvalidInOut);
        }
        self.in_point = Some(time);
        Ok(())
    }

    pub fn set_out_point(&mut self, time: Time) -> Result<(), EditError> {
        let time = time.max(Time::ZERO);
        if self.in_point.is_some_and(|start| time <= start) {
            return Err(EditError::InvalidInOut);
        }
        self.out_point = Some(time);
        Ok(())
    }

    pub fn clear_in_out(&mut self) {
        self.in_point = None;
        self.out_point = None;
    }

    fn insert_marker(&mut self, marker: Marker) {
        let index = self
            .markers
            .partition_point(|other| (other.time, other.id) <= (marker.time, marker.id));
        self.markers.insert(index, marker);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(seconds: i64) -> Time {
        Time::from_seconds(seconds)
    }

    fn times(project: &Project) -> Vec<Time> {
        project.markers.iter().map(|marker| marker.time).collect()
    }

    #[test]
    fn markers_stay_ordered_by_time_and_get_ids_that_are_never_reused() {
        let mut project = Project::new("test");

        let late = project.add_marker(seconds(9), "late");
        let early = project.add_marker(seconds(-3), "early");
        let middle = project.add_marker(seconds(4), "middle");

        assert_eq!(times(&project), [0, 4, 9].map(seconds));
        assert_eq!(
            project.markers.iter().map(|m| m.id).collect::<Vec<_>>(),
            [early, middle, late]
        );

        project.remove_marker(middle).unwrap();
        let again = project.add_marker(seconds(4), "again");

        assert_ne!(again, middle);
        assert!(project.next_ids.has_issued_marker(middle));
    }

    #[test]
    fn moving_a_marker_reorders_it_and_renaming_keeps_its_place() {
        let mut project = Project::new("test");
        let first = project.add_marker(seconds(1), "a");
        let second = project.add_marker(seconds(2), "b");

        project.move_marker(first, seconds(5)).unwrap();
        project.rename_marker(second, "renamed").unwrap();

        assert_eq!(project.markers[0].id, second);
        assert_eq!(project.markers[0].name, "renamed");
        assert_eq!(project.marker(first).map(|m| m.time), Some(seconds(5)));
        assert_eq!(
            project.move_marker(MarkerId(9), Time::ZERO),
            Err(EditError::UnknownMarker(MarkerId(9)))
        );
        assert_eq!(
            project.rename_marker(MarkerId(9), "x"),
            Err(EditError::UnknownMarker(MarkerId(9)))
        );
        assert_eq!(
            project.remove_marker(MarkerId(9)),
            Err(EditError::UnknownMarker(MarkerId(9)))
        );
    }

    #[test]
    fn neighbouring_markers_skip_the_one_at_the_time() {
        let mut project = Project::new("test");
        for second in [2, 4, 6] {
            project.add_marker(seconds(second), "");
        }

        let next = |time| project.next_marker_after(seconds(time)).map(|m| m.time);
        let previous = |time| {
            project
                .previous_marker_before(seconds(time))
                .map(|m| m.time)
        };

        assert_eq!(next(0), Some(seconds(2)));
        assert_eq!(next(2), Some(seconds(4)));
        assert_eq!(next(6), None);
        assert_eq!(previous(6), Some(seconds(4)));
        assert_eq!(previous(2), None);
        assert_eq!(previous(9), Some(seconds(6)));
    }

    #[test]
    fn the_in_point_stays_before_the_out_point() {
        let mut project = Project::new("test");

        project.set_in_point(seconds(2)).unwrap();
        project.set_out_point(seconds(8)).unwrap();

        assert_eq!(
            project.set_in_point(seconds(8)),
            Err(EditError::InvalidInOut)
        );
        assert_eq!(
            project.set_out_point(seconds(2)),
            Err(EditError::InvalidInOut)
        );
        assert_eq!(project.in_point, Some(seconds(2)));
        assert_eq!(project.out_point, Some(seconds(8)));

        project.set_in_point(seconds(-4)).unwrap();

        assert_eq!(project.in_point, Some(Time::ZERO));

        project.clear_in_out();

        assert_eq!((project.in_point, project.out_point), (None, None));
    }
}
