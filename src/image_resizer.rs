use fast_image_resize::{
    PixelType, Resizer,
    images::{Image, ImageRef},
};
use image::DynamicImage;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

#[derive(Debug)]
pub struct ImageResizer<T: Send + 'static> {
    sender: UnboundedSender<(T, Image<'static>)>,
    receiver: UnboundedReceiver<(T, Image<'static>)>,
}

impl<T: Send + 'static> ImageResizer<T> {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver }
    }

    pub fn resize_image(&mut self, key: T, src_image: DynamicImage, destination: (u32, u32)) {
        let (dst_width, dst_height) = destination;
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let mut dst_image = Image::new(dst_width, dst_height, PixelType::U8x4);

            tracing::trace!(
                "attempting to resize image! {}x{} => {}x{}",
                src_image.width(),
                src_image.height(),
                dst_width,
                dst_height
            );

            let mut resizer = Resizer::new();
            if let Err(err) = resizer.resize(&src_image, &mut dst_image, None) {
                tracing::warn!("failed to resize image! {}", err);
                return;
            }

            let _ = sender.send((key, dst_image));
        });
    }

    pub fn resize_bgra_pixels(&mut self, key: T, source: (Vec<u8>, u32), destination: (u32, u32)) {
        let (mut pixels, width) = source;
        let height = pixels.len() as u32 / width / 4;

        let (dst_width, dst_height) = destination;
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let src_image = match ImageRef::new(width, height, &mut pixels, PixelType::U8x4) {
                Ok(image_ref) => image_ref,
                Err(err) => {
                    tracing::warn!("Failed to read pixels as image: {}", err);
                    return;
                }
            };
            let mut dst_image = Image::new(dst_width, dst_height, PixelType::U8x4);

            tracing::trace!(
                "attempting to resize image! {}x{} => {}x{}",
                width,
                height,
                dst_width,
                dst_height
            );

            let mut resizer = Resizer::new();
            if let Err(err) = resizer.resize(&src_image, &mut dst_image, None) {
                tracing::warn!("failed to resize image! {}", err);
                return;
            }

            for chunk in dst_image.buffer_mut().chunks_exact_mut(4) {
                chunk.swap(0, 2);
            }

            let _ = sender.send((key, dst_image));
        });
    }

    pub async fn recv(&mut self) -> Option<(T, Image<'static>)> {
        self.receiver.recv().await
    }
}
