use tessera_render::Placement;
use tessera_timeline::{Opacity, Transform};

pub fn placement(transform: Transform, opacity: Opacity, (width, height): (u32, u32)) -> Placement {
    let permille = |value: u16| f32::from(value) / 1000.;
    Placement {
        offset: [
            transform.x as f32 / width.max(1) as f32,
            transform.y as f32 / height.max(1) as f32,
        ],
        scale: transform.scale as f32 / Transform::FULL_SCALE as f32,
        rotation_degrees: transform.rotation as f32 / 10.,
        crop: [
            transform.crop.left,
            transform.crop.top,
            transform.crop.right,
            transform.crop.bottom,
        ]
        .map(permille),
        opacity: opacity.fraction(),
    }
}

#[cfg(test)]
mod tests {
    use tessera_timeline::Crop;

    use super::*;

    #[test]
    fn a_clip_transform_becomes_a_placement_in_fractions_of_the_sequence() {
        let transform = Transform {
            x: 480,
            y: -270,
            scale: 1_500,
            rotation: -450,
            crop: Crop {
                left: 100,
                top: 0,
                right: 250,
                bottom: 0,
            },
        };

        let placed = placement(
            transform,
            Opacity::from_permille(600).unwrap(),
            (1920, 1080),
        );

        assert_eq!(placed.offset, [0.25, -0.25]);
        assert_eq!(placed.scale, 1.5);
        assert_eq!(placed.rotation_degrees, -45.);
        assert_eq!(placed.crop, [0.1, 0., 0.25, 0.]);
        assert_eq!(placed.opacity, 0.6);
        assert!(placement(Transform::IDENTITY, Opacity::OPAQUE, (1920, 1080)).is_fit());
    }
}
