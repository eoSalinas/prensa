//! Optimization settings and their builder.

/// An e-reader prensa has a profile for.
///
/// A profile is the box images are fitted into: a bit smaller than the screen,
/// since the reader keeps margins around the page. Pass one to
/// [`OptionsBuilder::device`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Device {
    /// Xteink X4 (screen 480x800): images fit 440x733. Chosen in a side-by-side
    /// comparison on an X4 Pro running CrossPoint. The default.
    #[default]
    X4,
    /// Xteink X3 (screen 528x792): images fit 484x726, the X4 box scaled to
    /// this screen (same margins, relative to the screen). Not yet compared
    /// on the device.
    X3,
}

impl Device {
    /// Largest image, as (width, height) in pixels.
    pub fn max_size(self) -> (u32, u32) {
        match self {
            Device::X4 => (440, 733),
            Device::X3 => (484, 726),
        }
    }
}

/// Optimization settings, always valid.
///
/// Use [`Options::default`] for the defaults, or [`Options::builder`] to change
/// some of them; [`OptionsBuilder::build`] is the only place values are
/// checked, so an `Options` never holds an out-of-range value.
///
/// ```
/// let opts = prensa::Options::builder()
///     .max_size(600, 800)
///     .jpeg_quality(85)
///     .build()?;
/// assert_eq!(opts.max_width(), 600);
/// assert_eq!(opts.sharpen(), Some(0.8)); // not set: the default
/// # Ok::<(), prensa::InvalidOptions>(())
/// ```
///
/// The defaults won a side-by-side comparison on an Xteink X4 Pro (CrossPoint):
/// Q85, Q92, Q92 + sharpening and Q92 + sharpening + 440 px on the same diagram
/// page; the last one won. 440x733 sits close to the reader viewport (480x800
/// minus bezel and margins), so the device does not resample again with its
/// low-quality fixed-point scaler.
///
/// Fields are private so their representation can change without breaking
/// callers. `Clone` but deliberately not `Copy`, so options that are not `Copy`
/// (a list of entries to skip, a device profile name) can be added later.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    sharpen: Option<f32>,
}

impl Options {
    /// A builder starting from the defaults.
    pub fn builder() -> OptionsBuilder {
        OptionsBuilder::default()
    }

    /// Maximum image width, in pixels.
    pub fn max_width(&self) -> u32 {
        self.max_width
    }

    /// Maximum image height, in pixels.
    pub fn max_height(&self) -> u32 {
        self.max_height
    }

    /// JPEG quality, 1-100.
    pub fn jpeg_quality(&self) -> u8 {
        self.jpeg_quality
    }

    /// Unsharp mask sigma applied after downscaling, or `None` when sharpening
    /// is off.
    pub fn sharpen(&self) -> Option<f32> {
        self.sharpen
    }
}

impl Default for Options {
    fn default() -> Self {
        let (max_width, max_height) = Device::default().max_size();
        Self {
            max_width,
            max_height,
            jpeg_quality: 92,
            sharpen: Some(0.8),
        }
    }
}

/// Builds an [`Options`]. Settings not given keep their default.
///
/// Setters take and return the builder, so they chain; [`build`](Self::build)
/// checks every value at once.
#[derive(Debug, Clone, Default, PartialEq)]
#[must_use = "a builder does nothing until `build` is called"]
pub struct OptionsBuilder {
    // Not validated yet; never handed out until `build` checks it.
    opts: Options,
}

impl OptionsBuilder {
    /// Fit images inside `width` x `height` pixels (aspect ratio kept, never
    /// upscaled). Both must be at least 1.
    pub fn max_size(mut self, width: u32, height: u32) -> Self {
        self.opts.max_width = width;
        self.opts.max_height = height;
        self
    }

    /// Fit images to the profile of `device`. The same as
    /// [`max_size`](Self::max_size) with [`Device::max_size`]; a later
    /// `max_size` call overrides it.
    pub fn device(self, device: Device) -> Self {
        let (width, height) = device.max_size();
        self.max_size(width, height)
    }

    /// JPEG quality, 1-100.
    pub fn jpeg_quality(mut self, quality: u8) -> Self {
        self.opts.jpeg_quality = quality;
        self
    }

    /// Sharpen images after downscaling with an unsharp mask of this sigma,
    /// finite and greater than 0. Use [`no_sharpen`](Self::no_sharpen) to turn
    /// sharpening off.
    pub fn sharpen(mut self, sigma: f32) -> Self {
        self.opts.sharpen = Some(sigma);
        self
    }

    /// Do not sharpen images. Sharpening is on by default.
    pub fn no_sharpen(mut self) -> Self {
        self.opts.sharpen = None;
        self
    }

    /// Check every setting and return the [`Options`].
    ///
    /// # Errors
    ///
    /// [`InvalidOptions`] names the first setting out of range.
    pub fn build(self) -> Result<Options, InvalidOptions> {
        let o = self.opts;
        if o.max_width == 0 || o.max_height == 0 {
            return Err(InvalidOptions::MaxSize {
                width: o.max_width,
                height: o.max_height,
            });
        }
        if !(1..=100).contains(&o.jpeg_quality) {
            return Err(InvalidOptions::JpegQuality(o.jpeg_quality));
        }
        if let Some(sigma) = o.sharpen
            && !(sigma.is_finite() && sigma > 0.0)
        {
            return Err(InvalidOptions::Sharpen(sigma));
        }
        Ok(o)
    }
}

/// Why [`OptionsBuilder::build`] refused the settings.
///
/// Its own type rather than a [`prensa::Error`](crate::Error) variant: building
/// options never touches a file, so callers only handle what can happen here.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum InvalidOptions {
    /// Maximum image size with a zero side.
    #[error("maximum image size must be at least 1x1, got {width}x{height}")]
    MaxSize {
        /// Requested maximum width.
        width: u32,
        /// Requested maximum height.
        height: u32,
    },
    /// JPEG quality outside 1-100.
    #[error("JPEG quality must be between 1 and 100, got {0}")]
    JpegQuality(u8),
    /// Sharpening sigma that is not finite or not greater than 0.
    #[error("sharpening sigma must be finite and greater than 0, got {0}")]
    Sharpen(f32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_starts_from_the_defaults() {
        assert_eq!(Options::builder().build(), Ok(Options::default()));
    }

    #[test]
    fn setters_change_only_their_setting() {
        let opts = Options::builder()
            .max_size(600, 800)
            .jpeg_quality(85)
            .no_sharpen()
            .build()
            .unwrap();
        assert_eq!((opts.max_width(), opts.max_height()), (600, 800));
        assert_eq!(opts.jpeg_quality(), 85);
        assert_eq!(opts.sharpen(), None);

        let sharp = Options::builder().sharpen(1.5).build().unwrap();
        assert_eq!(sharp.sharpen(), Some(1.5));
        assert_eq!(sharp.max_width(), Options::default().max_width());
    }

    #[test]
    fn device_sets_the_image_box() {
        let x3 = Options::builder().device(Device::X3).build().unwrap();
        assert_eq!((x3.max_width(), x3.max_height()), (484, 726));
        let x4 = Options::builder().device(Device::X4).build().unwrap();
        assert_eq!(x4, Options::default());
        // A later max_size wins.
        let custom = Options::builder()
            .device(Device::X3)
            .max_size(600, 800)
            .build()
            .unwrap();
        assert_eq!((custom.max_width(), custom.max_height()), (600, 800));
    }

    #[test]
    fn x3_box_keeps_the_x4_margins() {
        // 440/480 and 733/800 of the screen, applied to 528x792.
        let (w, h) = Device::X3.max_size();
        assert_eq!(w, (528.0_f64 * 440.0 / 480.0).round() as u32);
        assert_eq!(h, (792.0_f64 * 733.0 / 800.0).round() as u32);
    }

    #[test]
    fn build_rejects_out_of_range_settings() {
        let b = Options::builder;
        assert_eq!(
            b().max_size(0, 800).build(),
            Err(InvalidOptions::MaxSize {
                width: 0,
                height: 800
            })
        );
        assert!(matches!(
            b().max_size(600, 0).build(),
            Err(InvalidOptions::MaxSize { .. })
        ));
        assert_eq!(
            b().jpeg_quality(0).build(),
            Err(InvalidOptions::JpegQuality(0))
        );
        assert_eq!(
            b().jpeg_quality(101).build(),
            Err(InvalidOptions::JpegQuality(101))
        );
        for sigma in [0.0, -1.0, f32::INFINITY, f32::NAN] {
            assert!(
                matches!(b().sharpen(sigma).build(), Err(InvalidOptions::Sharpen(_))),
                "sigma {sigma}"
            );
        }
    }

    #[test]
    fn errors_name_the_value() {
        let err = Options::builder().jpeg_quality(150).build().unwrap_err();
        assert_eq!(
            err.to_string(),
            "JPEG quality must be between 1 and 100, got 150"
        );
    }
}
