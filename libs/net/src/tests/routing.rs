//! The routing table: which route a destination takes, and in what order.

use crate::addr::{IpAddress, IpCidr, Ipv4, Ipv6};
use crate::route::{Origin, Route, Routes};

/// A route to a prefix by an interface, with a metric.
fn route(prefix: [u8; 4], len: u8, interface: u32, metric: u32) -> Route {
    Route {
        destination: IpCidr::new(IpAddress::V4(Ipv4::new(prefix)), len),
        gateway: None,
        interface,
        metric,
        origin: Origin::Static,
    }
}

#[test]
fn the_longest_prefix_wins() {
    let mut routes = Routes::new();
    routes.add(route([0, 0, 0, 0], 0, 1, 0));
    routes.add(route([10, 0, 0, 0], 8, 2, 0));
    routes.add(route([10, 1, 0, 0], 16, 3, 0));

    let hop = routes
        .lookup(IpAddress::V4(Ipv4::new([10, 1, 2, 3])))
        .expect("a default route matches everything");
    assert_eq!(hop.interface, 3);

    let hop = routes
        .lookup(IpAddress::V4(Ipv4::new([10, 9, 9, 9])))
        .expect("the eight-bit prefix matches");
    assert_eq!(hop.interface, 2);

    let hop = routes
        .lookup(IpAddress::V4(Ipv4::new([8, 8, 8, 8])))
        .expect("the default route matches");
    assert_eq!(hop.interface, 1);
}

#[test]
fn among_equal_prefixes_the_lowest_metric_wins() {
    let mut routes = Routes::new();
    routes.add(route([0, 0, 0, 0], 0, 1, 200));
    routes.add(route([0, 0, 0, 0], 0, 2, 50));
    let hop = routes
        .lookup(IpAddress::V4(Ipv4::new([1, 1, 1, 1])))
        .expect("there is a default route");
    assert_eq!(hop.interface, 2);
}

#[test]
fn a_gateway_is_what_gets_resolved_rather_than_the_destination() {
    let mut routes = Routes::new();
    let gateway = IpAddress::V4(Ipv4::new([10, 0, 0, 254]));
    routes.add(Route {
        destination: IpCidr::new(IpAddress::V4(Ipv4::UNSPECIFIED), 0),
        gateway: Some(gateway),
        interface: 2,
        metric: 0,
        origin: Origin::Static,
    });
    let hop = routes
        .lookup(IpAddress::V4(Ipv4::new([93, 184, 216, 34])))
        .expect("the default route matches");
    assert_eq!(hop.address, gateway);
    assert!(!hop.on_link);
}

#[test]
fn an_on_link_route_resolves_the_destination_itself() {
    let mut routes = Routes::new();
    routes.add(route([10, 0, 0, 0], 24, 2, 0));
    let destination = IpAddress::V4(Ipv4::new([10, 0, 0, 7]));
    let hop = routes.lookup(destination).expect("the prefix matches");
    assert_eq!(hop.address, destination);
    assert!(hop.on_link);
}

#[test]
fn a_route_that_was_removed_no_longer_matches() {
    let mut routes = Routes::new();
    routes.add(route([10, 0, 0, 0], 8, 2, 0));
    let prefix = IpCidr::new(IpAddress::V4(Ipv4::new([10, 0, 0, 0])), 8);
    assert!(routes.remove(prefix, 2, None));
    assert!(
        routes
            .lookup(IpAddress::V4(Ipv4::new([10, 1, 1, 1])))
            .is_none()
    );
    assert!(!routes.remove(prefix, 2, None));
}

#[test]
fn taking_an_interface_down_takes_its_routes_with_it() {
    let mut routes = Routes::new();
    routes.add(route([10, 0, 0, 0], 8, 2, 0));
    routes.add(route([192, 168, 0, 0], 16, 3, 0));
    routes.remove_interface(2);
    assert!(
        routes
            .lookup(IpAddress::V4(Ipv4::new([10, 1, 1, 1])))
            .is_none()
    );
    assert!(
        routes
            .lookup(IpAddress::V4(Ipv4::new([192, 168, 1, 1])))
            .is_some()
    );
}

#[test]
fn the_two_families_do_not_route_each_other() {
    let mut routes = Routes::new();
    routes.add(route([0, 0, 0, 0], 0, 1, 0));
    assert!(routes.lookup(IpAddress::V6(Ipv6::LOOPBACK)).is_none());
}
