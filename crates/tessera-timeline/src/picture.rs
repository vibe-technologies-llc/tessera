use std::fmt;

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Transform {
    pub x: i32,
    pub y: i32,
    pub scale: u32,
    pub rotation: i32,
    pub crop: Crop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Crop {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransformField {
    X,
    Y,
    Scale,
    Rotation,
    CropLeft,
    CropTop,
    CropRight,
    CropBottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum InvalidTransform {
    #[error("the position {0} pixels lies too far from the centre")]
    Position(i32),
    #[error("the scale {0}‰ is not between 1‰ and 10 000‰")]
    Scale(u32),
    #[error("the rotation {0} tenths of a degree is more than a whole turn")]
    Rotation(i32),
    #[error("the crop leaves nothing of the picture")]
    Crop(Crop),
}

impl Transform {
    pub const IDENTITY: Self = Self {
        x: 0,
        y: 0,
        scale: Self::FULL_SCALE,
        rotation: 0,
        crop: Crop::NONE,
    };
    pub const FULL_SCALE: u32 = 1_000;
    pub const MIN_SCALE: u32 = 1;
    pub const MAX_SCALE: u32 = 10_000;
    pub const MAX_OFFSET: i32 = 100_000;
    pub const WHOLE_TURN: i32 = 3_600;

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    pub fn check(&self) -> Result<(), InvalidTransform> {
        if let Some(&offset) = [self.x, self.y]
            .iter()
            .find(|offset| offset.unsigned_abs() > Self::MAX_OFFSET.unsigned_abs())
        {
            Err(InvalidTransform::Position(offset))
        } else if !(Self::MIN_SCALE..=Self::MAX_SCALE).contains(&self.scale) {
            Err(InvalidTransform::Scale(self.scale))
        } else if self.rotation.unsigned_abs() > Self::WHOLE_TURN.unsigned_abs() {
            Err(InvalidTransform::Rotation(self.rotation))
        } else if !self.crop.leaves_picture() {
            Err(InvalidTransform::Crop(self.crop))
        } else {
            Ok(())
        }
    }

    pub fn get(&self, field: TransformField) -> i64 {
        match field {
            TransformField::X => self.x.into(),
            TransformField::Y => self.y.into(),
            TransformField::Scale => self.scale.into(),
            TransformField::Rotation => self.rotation.into(),
            TransformField::CropLeft => self.crop.left.into(),
            TransformField::CropTop => self.crop.top.into(),
            TransformField::CropRight => self.crop.right.into(),
            TransformField::CropBottom => self.crop.bottom.into(),
        }
    }

    pub fn with(self, field: TransformField, value: i64) -> Self {
        let offset = |value: i64| {
            value.clamp(-i64::from(Self::MAX_OFFSET), i64::from(Self::MAX_OFFSET)) as i32
        };
        let mut changed = self;
        match field {
            TransformField::X => changed.x = offset(value),
            TransformField::Y => changed.y = offset(value),
            TransformField::Scale => {
                changed.scale =
                    value.clamp(i64::from(Self::MIN_SCALE), i64::from(Self::MAX_SCALE)) as u32;
            }
            TransformField::Rotation => {
                let turn = i64::from(Self::WHOLE_TURN);
                changed.rotation = value.clamp(-turn, turn) as i32;
            }
            TransformField::CropLeft => {
                changed.crop.left = Crop::side(value, self.crop.right);
            }
            TransformField::CropTop => changed.crop.top = Crop::side(value, self.crop.bottom),
            TransformField::CropRight => {
                changed.crop.right = Crop::side(value, self.crop.left);
            }
            TransformField::CropBottom => {
                changed.crop.bottom = Crop::side(value, self.crop.top);
            }
        }
        changed
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl TransformField {
    pub const ALL: [Self; 8] = [
        Self::X,
        Self::Y,
        Self::Scale,
        Self::Rotation,
        Self::CropLeft,
        Self::CropTop,
        Self::CropRight,
        Self::CropBottom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::X => "Position X",
            Self::Y => "Position Y",
            Self::Scale => "Scale",
            Self::Rotation => "Rotation",
            Self::CropLeft => "Crop Left",
            Self::CropTop => "Crop Top",
            Self::CropRight => "Crop Right",
            Self::CropBottom => "Crop Bottom",
        }
    }
}

impl Crop {
    pub const NONE: Self = Self {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    pub const WHOLE: u16 = 1_000;

    pub fn leaves_picture(&self) -> bool {
        u32::from(self.left) + u32::from(self.right) < u32::from(Self::WHOLE)
            && u32::from(self.top) + u32::from(self.bottom) < u32::from(Self::WHOLE)
    }

    fn side(value: i64, opposite: u16) -> u16 {
        let most = i64::from(Self::WHOLE) - 1 - i64::from(opposite);
        value.clamp(0, most.max(0)) as u16
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Opacity(u16);

impl Opacity {
    pub const TRANSPARENT: Self = Self(0);
    pub const OPAQUE: Self = Self(1_000);

    pub fn from_permille(permille: u16) -> Option<Self> {
        (permille <= Self::OPAQUE.0).then_some(Self(permille))
    }

    pub fn saturating(permille: i64) -> Self {
        Self(permille.clamp(0, i64::from(Self::OPAQUE.0)) as u16)
    }

    pub fn permille(self) -> u16 {
        self.0
    }

    pub fn fraction(self) -> f32 {
        f32::from(self.0) / f32::from(Self::OPAQUE.0)
    }
}

impl Default for Opacity {
    fn default() -> Self {
        Self::OPAQUE
    }
}

impl fmt::Display for Opacity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", f64::from(self.0) / 10.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_is_valid_and_out_of_range_fields_are_not() {
        assert_eq!(Transform::IDENTITY.check(), Ok(()));
        assert!(Transform::default().is_identity());

        let scaled = Transform {
            scale: 0,
            ..Transform::IDENTITY
        };
        let turned = Transform {
            rotation: -3_601,
            ..Transform::IDENTITY
        };
        let cropped = Transform {
            crop: Crop {
                left: 600,
                right: 400,
                ..Crop::NONE
            },
            ..Transform::IDENTITY
        };
        let distant = Transform {
            y: 100_001,
            ..Transform::IDENTITY
        };

        assert_eq!(scaled.check(), Err(InvalidTransform::Scale(0)));
        assert_eq!(turned.check(), Err(InvalidTransform::Rotation(-3_601)));
        assert_eq!(cropped.check(), Err(InvalidTransform::Crop(cropped.crop)));
        assert_eq!(distant.check(), Err(InvalidTransform::Position(100_001)));
    }

    #[test]
    fn setting_a_field_clamps_it_into_range() {
        let base = Transform {
            crop: Crop {
                right: 300,
                ..Crop::NONE
            },
            ..Transform::IDENTITY
        };

        assert_eq!(
            base.with(TransformField::Scale, 0).scale,
            Transform::MIN_SCALE
        );
        assert_eq!(base.with(TransformField::Rotation, 9_000).rotation, 3_600);
        assert_eq!(base.with(TransformField::CropLeft, 900).crop.left, 699);
        assert_eq!(base.with(TransformField::X, -5).get(TransformField::X), -5);
        for field in TransformField::ALL {
            assert_eq!(base.with(field, i64::MAX).check(), Ok(()), "{field:?}");
            assert_eq!(base.with(field, i64::MIN).check(), Ok(()), "{field:?}");
        }
    }

    #[test]
    fn opacity_lies_between_transparent_and_opaque() {
        assert_eq!(Opacity::from_permille(1_001), None);
        assert_eq!(Opacity::saturating(-4), Opacity::TRANSPARENT);
        assert_eq!(Opacity::saturating(5_000), Opacity::OPAQUE);
        assert_eq!(Opacity::from_permille(255).unwrap().to_string(), "25.5%");
        assert_eq!(Opacity::default().fraction(), 1.0);
    }
}
