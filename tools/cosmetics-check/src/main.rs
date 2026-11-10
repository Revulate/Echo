//! Contract: 7TV user-profile, cosmetics-catalog and v4 paint parsing, paint/badge
//! resolution and gradient-layer decoding in `crates/echo-cosmetics-core`.

use check_support::{FIXTURE_HEADER, assert_fixture, bool_text};
use echo_cosmetics_core::{
    ResolvedUserCosmetics, RgbaColor, SevenTvGradientKind, SevenTvGradientStop, SevenTvNamePaint,
    SevenTvRadialShape, SevenTvUserProfile, parse_cosmetics_catalog_json, parse_paints_v4_json,
    parse_user_profile_json, repeating_gradient_ratio, resolve_user_cosmetics,
};

fn main() {
    let user_json = sample_user_json();
    let catalog_json = sample_catalog_json();
    let paints_v4_json = sample_paints_v4_json();

    let profile = parse_user_profile_json(user_json).expect("sample user profile should parse");
    assert_eq!(profile.id, "01H00000000000000000000001");
    assert_eq!(
        profile.style.color.expect("style color").to_hex_rgb(),
        "#17ebb6"
    );
    assert_eq!(
        profile.style.badge_id.as_deref(),
        Some("01GAF95ZTG000E8VNG1S1RMTBB")
    );

    let catalog = parse_cosmetics_catalog_json(catalog_json).expect("sample catalog should parse");
    let resolved = resolve_user_cosmetics(&profile, &catalog);
    assert_eq!(
        resolved.avatar_url.as_deref(),
        Some("https://static-cdn.jtvnw.net/jtv_user_pictures/example-profile_image-300x300.png")
    );
    let v3_paint = resolved.paint.as_ref().expect("paint");
    assert!(
        v3_paint.colors.is_empty(),
        "the 7TV profile role color must not be injected into a catalog paint"
    );
    let mut unpainted_profile = profile.clone();
    unpainted_profile.style.paint_id = None;
    assert!(
        resolve_user_cosmetics(&unpainted_profile, &catalog)
            .paint
            .is_none(),
        "the 7TV profile role color must not become a synthetic name paint"
    );
    assert_eq!(v3_paint.text.weight, Some(700));
    assert_eq!(v3_paint.transformed_display_name("alice"), "ALICE");
    let stroke = v3_paint
        .text
        .stroke
        .as_ref()
        .expect("paint stroke should parse");
    assert_eq!(stroke.color.to_hex_rgb(), "#fefeff");
    assert!((stroke.width - 1.5).abs() <= f32::EPSILON);

    let v4_paints = parse_paints_v4_json(paints_v4_json).expect("sample v4 paints should parse");
    let gradient_paint = v4_paints
        .get("01PAINT000000000000000000")
        .expect("v4 gradient paint fixture");
    let image_paint = v4_paints
        .get("01IMAGE000000000000000000")
        .expect("v4 image paint fixture");
    let radial_paint = v4_paints
        .get("01RADIAL00000000000000000")
        .expect("v4 radial paint fixture");

    let gradient = gradient_paint
        .gradients
        .first()
        .expect("v4 linear gradient layer");
    assert_eq!(gradient.kind, SevenTvGradientKind::Linear);
    assert!((gradient.angle_degrees - 45.0).abs() <= f32::EPSILON);
    assert!(gradient.repeat);
    assert_eq!(gradient.stops.len(), 3);
    assert!((gradient.opacity - 0.9).abs() <= f32::EPSILON);

    let repeating_stops = [
        SevenTvGradientStop {
            at: 0.0,
            color: RgbaColor {
                r: 31,
                g: 89,
                b: 224,
                a: 255,
            },
        },
        SevenTvGradientStop {
            at: 0.21,
            color: RgbaColor {
                r: 0,
                g: 251,
                b: 255,
                a: 255,
            },
        },
        SevenTvGradientStop {
            at: 0.35,
            color: RgbaColor {
                r: 0,
                g: 95,
                b: 143,
                a: 255,
            },
        },
    ];
    let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
    assert!(close(
        repeating_gradient_ratio(&repeating_stops, 0.36),
        0.01
    ));
    assert!(close(repeating_gradient_ratio(&repeating_stops, 0.35), 0.0));
    let repeating_period_sample = repeating_gradient_ratio(&repeating_stops, 1.0);
    assert!(close(repeating_period_sample, 0.30));
    let offset_stops = [
        SevenTvGradientStop {
            at: 0.2,
            color: RgbaColor::default(),
        },
        SevenTvGradientStop {
            at: 0.4,
            color: RgbaColor::default(),
        },
    ];
    assert!(close(repeating_gradient_ratio(&offset_stops, 0.1), 0.3));
    let degenerate = [
        SevenTvGradientStop {
            at: 0.5,
            color: RgbaColor::default(),
        },
        SevenTvGradientStop {
            at: 0.5,
            color: RgbaColor::default(),
        },
    ];
    assert!(close(repeating_gradient_ratio(&degenerate, 0.9), 0.9));
    let unsorted_stops = [
        SevenTvGradientStop {
            at: 0.8,
            color: RgbaColor::default(),
        },
        SevenTvGradientStop {
            at: 0.2,
            color: RgbaColor::default(),
        },
    ];
    assert!(close(repeating_gradient_ratio(&unsorted_stops, 1.25), 1.0));
    let single_stop = [SevenTvGradientStop {
        at: 0.5,
        color: RgbaColor::default(),
    }];
    assert!(close(repeating_gradient_ratio(&single_stop, -0.25), 0.0));
    assert!(close(repeating_gradient_ratio(&[], 1.25), 1.0));
    let shadow = gradient_paint
        .shadows
        .first()
        .expect("v4 shadow should parse");
    assert!((shadow.x_offset - 1.0).abs() <= f32::EPSILON);
    assert!((shadow.y_offset - 2.0).abs() <= f32::EPSILON);
    assert!((shadow.radius - 3.0).abs() <= f32::EPSILON);

    let radial = radial_paint
        .gradients
        .first()
        .expect("v4 radial gradient layer");
    assert_eq!(
        radial.kind,
        SevenTvGradientKind::Radial(SevenTvRadialShape::Ellipse)
    );

    assert_eq!(
        image_paint.image_urls.first().map(String::as_str),
        Some("https://cdn.7tv.app/paint/image-2x-animated.webp")
    );
    assert!(
        image_paint
            .gradients
            .first()
            .is_some_and(|layer| layer.image_is_animated),
        "v4 frameCount metadata must survive URL selection"
    );
    assert!(
        image_paint
            .representative_color()
            .expect("image paint fallback color")
            .a
            > 0
    );

    println!("{FIXTURE_HEADER}");
    print_smoke_rows(
        &profile,
        &resolved,
        v3_paint,
        gradient_paint,
        image_paint,
        repeating_period_sample,
    );
}

fn print_smoke_rows(
    profile: &SevenTvUserProfile,
    resolved: &ResolvedUserCosmetics,
    v3_paint: &SevenTvNamePaint,
    gradient_paint: &SevenTvNamePaint,
    image_paint: &SevenTvNamePaint,
    repeating_period_sample: f32,
) {
    assert_fixture(
        "cosmetics_profile_id",
        &profile.id,
        "01H00000000000000000000001",
    );
    assert_fixture(
        "cosmetics_profile_color",
        profile
            .style
            .color
            .map(|color| color.to_hex_rgb())
            .unwrap_or_else(|| "none".to_string()),
        "#17ebb6",
    );
    assert_fixture(
        "cosmetics_profile_badge_id",
        profile.style.badge_id.as_deref().unwrap_or("none"),
        "01GAF95ZTG000E8VNG1S1RMTBB",
    );
    assert_fixture(
        "cosmetics_avatar_url",
        resolved.avatar_url.as_deref().unwrap_or("none"),
        "https://static-cdn.jtvnw.net/jtv_user_pictures/example-profile_image-300x300.png",
    );
    assert_fixture(
        "cosmetics_v4_gradient_stop_count",
        gradient_paint
            .gradients
            .first()
            .map(|gradient| gradient.stops.len())
            .unwrap_or_default()
            .to_string(),
        "3",
    );
    assert_fixture(
        "cosmetics_v4_gradient_angle",
        format!(
            "{:.1}",
            gradient_paint
                .gradients
                .first()
                .map(|gradient| gradient.angle_degrees)
                .unwrap_or_default()
        ),
        "45.0",
    );
    assert_fixture(
        "cosmetics_repeating_period",
        format!("{repeating_period_sample:.2}"),
        "0.30",
    );
    assert_fixture(
        "cosmetics_v4_shadow_blur",
        format!(
            "{:.1}",
            gradient_paint
                .shadows
                .first()
                .map(|shadow| shadow.radius)
                .unwrap_or_default()
        ),
        "3.0",
    );
    assert_fixture(
        "cosmetics_shadow_render_budget",
        echo_cosmetics_core::MAX_7TV_SHADOW_RENDER_LAYERS.to_string(),
        "8",
    );
    assert_fixture(
        "cosmetics_v4_image_paint_url",
        image_paint
            .image_urls
            .first()
            .map(String::as_str)
            .unwrap_or("none"),
        "https://cdn.7tv.app/paint/image-2x-animated.webp",
    );
    assert_fixture(
        "cosmetics_v4_image_paint_animated",
        image_paint
            .gradients
            .first()
            .is_some_and(|layer| layer.image_is_animated)
            .to_string(),
        "true",
    );
    assert_fixture(
        "cosmetics_text_weight",
        v3_paint.text.weight.unwrap_or_default().to_string(),
        "700",
    );
    assert_fixture(
        "cosmetics_text_transform",
        v3_paint.transformed_display_name("alice"),
        "ALICE",
    );
    let stroke = v3_paint.text.stroke.as_ref().expect("paint stroke");
    assert_fixture(
        "cosmetics_stroke_color",
        stroke.color.to_hex_rgb(),
        "#fefeff",
    );
    assert_fixture(
        "cosmetics_stroke_width",
        format!("{:.1}", stroke.width),
        "1.5",
    );
    let badge = resolved.badge.as_ref().expect("badge");
    assert_fixture(
        "cosmetics_badge_url",
        &badge.url,
        "https://cdn.7tv.app/badge/01GAF95ZTG000E8VNG1S1RMTBB/2x.webp",
    );
    assert_fixture(
        "cosmetics_badge_tooltip",
        &badge.tooltip,
        "7TV Subscriber (2 Months)",
    );
    assert_fixture(
        "cosmetics_badge_animated",
        bool_text(badge.is_animated),
        "false",
    );
}

fn sample_user_json() -> &'static str {
    r##"{
  "id": "12345678",
  "platform": "TWITCH",
  "username": "alice",
  "display_name": "alice",
  "user": {
    "id": "01H00000000000000000000001",
    "username": "alice",
    "display_name": "alice",
    "avatar_url": "https://static-cdn.jtvnw.net/jtv_user_pictures/example-profile_image-300x300.png",
    "style": {
      "color": 401323775,
      "paint_id": "01PAINT000000000000000000",
      "badge_id": "01GAF95ZTG000E8VNG1S1RMTBB"
    }
  }
}"##
}

fn sample_catalog_json() -> &'static str {
    r##"{
  "data": {
    "cosmetics": {
      "paints": [
        {
          "id": "01PAINT000000000000000000",
          "name": "Echo Gradient",
          "color": null,
          "gradients": [],
          "shadows": [
            { "x_offset": 1.0, "y_offset": 2.0, "radius": 3.0, "color": 624034303 }
          ],
          "text": {
            "weight": 700,
            "transform": "UPPERCASE",
            "variant": "default",
            "shadows": [
              { "x_offset": -1.0, "y_offset": 1.0, "radius": 2.0, "color": 169090815 }
            ],
            "stroke": {
              "color": -16842753,
              "width": 1.5
            }
          }
        }
      ],
      "badges": [
        {
          "id": "01GAF95ZTG000E8VNG1S1RMTBB",
          "name": "7TV Subscriber - 2 Months",
          "tooltip": "7TV Subscriber (2 Months)",
          "host": {
            "url": "//cdn.7tv.app/badge/01GAF95ZTG000E8VNG1S1RMTBB",
            "files": [
              { "name": "1x.webp", "frame_count": 1, "format": "WEBP" },
              { "name": "2x.webp", "frame_count": 1, "format": "WEBP" },
              { "name": "4x.webp", "frame_count": 1, "format": "WEBP" }
            ]
          }
        }
      ]
    }
  }
}"##
}

fn sample_paints_v4_json() -> &'static str {
    r##"{
  "data": {
    "paints": {
      "paints": [
        {
          "id": "01PAINT000000000000000000",
          "name": "Echo Gradient",
          "data": {
            "layers": [
              {
                "opacity": 0.9,
                "ty": {
                  "__typename": "PaintLayerTypeLinearGradient",
                  "angle": 45.0,
                  "repeating": true,
                  "stops": [
                    { "at": 0.0, "color": { "r": 255, "g": 0, "b": 255, "a": 255 } },
                    { "at": 0.5, "color": { "r": 0, "g": 255, "b": 255, "a": 255 } },
                    { "at": 1.0, "color": { "r": 255, "g": 255, "b": 0, "a": 255 } }
                  ]
                }
              }
            ],
            "shadows": [
              { "color": { "r": 37, "g": 50, "b": 1, "a": 255 }, "offsetX": 1.0, "offsetY": 2.0, "blur": 3.0 }
            ]
          }
        },
        {
          "id": "01RADIAL00000000000000000",
          "name": "Echo Radial",
          "data": {
            "layers": [
              {
                "opacity": 1.0,
                "ty": {
                  "__typename": "PaintLayerTypeRadialGradient",
                  "repeating": false,
                  "shape": "ELLIPSE",
                  "stops": [
                    { "at": 0.0, "color": { "r": 255, "g": 255, "b": 255, "a": 255 } },
                    { "at": 1.0, "color": { "r": 0, "g": 0, "b": 0, "a": 255 } }
                  ]
                }
              }
            ],
            "shadows": []
          }
        },
        {
          "id": "01IMAGE000000000000000000",
          "name": "Echo Image Fill",
          "data": {
            "layers": [
              {
                "opacity": 1.0,
                "ty": {
                  "__typename": "PaintLayerTypeImage",
                  "images": [
                    { "url": "https://cdn.7tv.app/paint/image-1x.webp", "scale": 1, "frameCount": 1 },
                    { "url": "https://cdn.7tv.app/paint/image-2x-animated.webp", "scale": 2, "frameCount": 12 }
                  ]
                }
              }
            ],
            "shadows": []
          }
        }
      ]
    }
  }
}"##
}
