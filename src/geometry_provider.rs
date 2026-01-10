pub type Geometry = (i32, i32, i32, i32);

pub trait GeometryProvider {
    fn new() -> anyhow::Result<Self>
    where
        Self: Sized;
    fn get_active_window_geometry(&mut self) -> anyhow::Result<Geometry>;
}
