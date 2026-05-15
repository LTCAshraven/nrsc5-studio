pub struct CollageEngine {
    _horizon_hours: u32,
}

impl CollageEngine {
    pub fn new(horizon_hours: u32) -> Self {
        Self {
            _horizon_hours: horizon_hours,
        }
    }
}
