//! The event encoding, against the layout of Linux's `union dwc3_event`.

use crate::event::{DeviceEvent, EndpointEvent, Event};
use crate::trb::{CONTROL_SETUP, HWO, IOC, LST, Trb};

#[test]
fn endpoint_events() {
    // XferComplete on physical 1, status short, parameter 0.
    assert_eq!(
        Event::decode((1 << 1) | (1 << 6) | (2 << 12)),
        Event::Endpoint {
            physical: 1,
            kind: EndpointEvent::XferComplete,
            status: 2,
            parameter: 0
        },
        "transfer complete"
    );
    // XferNotReady on physical 0 for the status stage.
    assert_eq!(
        Event::decode((3 << 6) | (2 << 12)),
        Event::Endpoint {
            physical: 0,
            kind: EndpointEvent::XferNotReady,
            status: 2,
            parameter: 0
        },
        "transfer not ready"
    );
    // End Transfer's command complete on physical 5.
    assert_eq!(
        Event::decode((5 << 1) | (7 << 6) | (0x0800 << 16)),
        Event::Endpoint {
            physical: 5,
            kind: EndpointEvent::CommandComplete,
            status: 0,
            parameter: 0x0800
        },
        "command complete"
    );
    assert_eq!(
        Event::decode(2 << 6),
        Event::Endpoint {
            physical: 0,
            kind: EndpointEvent::XferInProgress,
            status: 0,
            parameter: 0
        },
        "in progress"
    );
}

#[test]
fn device_events() {
    let device = |kind: u32, information: u32| Event::decode(1 | (kind << 8) | (information << 16));
    assert_eq!(
        device(0, 0),
        Event::Device(DeviceEvent::Disconnect),
        "disconnect"
    );
    assert_eq!(device(1, 0), Event::Device(DeviceEvent::Reset), "reset");
    assert_eq!(
        device(2, 0),
        Event::Device(DeviceEvent::ConnectDone),
        "connect done"
    );
    assert_eq!(
        device(3, 0x13),
        Event::Device(DeviceEvent::LinkStateChange(3)),
        "to U3"
    );
    assert_eq!(
        device(6, 3),
        Event::Device(DeviceEvent::Suspend(3)),
        "suspend"
    );
    assert_eq!(
        device(11, 0),
        Event::Device(DeviceEvent::Overflow),
        "overflow"
    );
    assert_eq!(
        device(12, 0),
        Event::Device(DeviceEvent::Other(12)),
        "vendor test"
    );
    assert_eq!(
        Event::decode(1 | (3 << 1)),
        Event::Other(7),
        "a carkit event"
    );
}

#[test]
fn trb_words() {
    let trb = Trb::new(0x1_2345_6780, 8, CONTROL_SETUP, LST | IOC);
    assert_eq!(trb.size, 8, "length");
    assert_eq!(trb.control, HWO | LST | IOC | (2 << 4), "control word");
    let done = Trb {
        size: 3 | (2 << 28),
        ..trb
    };
    assert_eq!(done.remaining(), 3, "left");
    assert_eq!(done.status(), 2, "setup pending");
}
