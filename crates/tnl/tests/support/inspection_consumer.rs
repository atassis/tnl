use std::sync::{Arc, Mutex};
use tnl::inspection::{
    CaptureContext, ConsumerFactory, Direction, Observation, ObservationConsumer,
};

#[derive(Default)]
pub struct Recorded {
    pub context: Option<CaptureContext>,
    pub request: Vec<u8>,
    pub response: Vec<u8>,
    pub head_input: Vec<u8>,
    pub statuses: Vec<u16>,
    pub ends: Vec<(Direction, bool)>,
    pub stops: Vec<(Direction, &'static str)>,
    pub finished: usize,
}
pub struct Recorder(pub Arc<Mutex<Recorded>>);
impl ConsumerFactory for Recorder {
    fn admit(&self, context: CaptureContext) -> Option<Box<dyn ObservationConsumer>> {
        self.0.lock().unwrap().context = Some(context);
        Some(Box::new(Self(self.0.clone())))
    }
}
impl ObservationConsumer for Recorder {
    fn observe(&mut self, event: &Observation<'_>) {
        let mut seen = self.0.lock().unwrap();
        match event {
            Observation::RequestHead {
                method,
                target,
                headers,
            } => {
                seen.head_input.extend_from_slice(method.as_bytes());
                seen.head_input.extend_from_slice(target.as_bytes());
                record_headers(&mut seen.head_input, headers);
            }
            Observation::ResponseHead { status, headers } => {
                seen.statuses.push(*status);
                record_headers(&mut seen.head_input, headers);
            }
            Observation::Body(Direction::Request, bytes) => seen.request.extend_from_slice(bytes),
            Observation::Body(Direction::Response, bytes) => seen.response.extend_from_slice(bytes),
            Observation::MessageEnd {
                direction,
                trailers,
            } => seen.ends.push((*direction, *trailers)),
            Observation::Stopped(direction, reason) => seen.stops.push((*direction, reason)),
            Observation::Finished => seen.finished += 1,
        }
        // This fixture is bounded too; test input must never grow recording
        // beyond the expected small exchanges or unlimited callback counts.
        assert!(seen.request.len() <= 128 * 1024 && seen.response.len() <= 128 * 1024);
        assert!(seen.head_input.len() <= 64 * 1024);
        assert!(seen.statuses.len() <= 16 && seen.stops.len() <= 16 && seen.ends.len() <= 16);
        drop(seen);
    }
}
fn record_headers(output: &mut Vec<u8>, headers: &[httparse::Header<'_>]) {
    for header in headers {
        output.extend_from_slice(header.name.as_bytes());
        output.extend_from_slice(header.value);
    }
}
