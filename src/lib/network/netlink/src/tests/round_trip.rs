//! What the builder writes walks back to what was built — the property the
//! `netlink_walk` fuzz target holds the crate to, asserted here over the
//! messages `ip` actually exchanges.

use alloc::vec::Vec;

use ferrix_linux_abi::netlink::{
    ARPHRD_ETHER, IFA_ADDRESS, IFA_LOCAL, IFF_BROADCAST, IFF_RUNNING, IFF_UP, IFLA_ADDRESS,
    IFLA_IFNAME, IFLA_MTU, IfAddrMsg, IfInfoMsg, NLM_F_MULTI, NlMsgHdr, RT_SCOPE_UNIVERSE,
    RT_TABLE_MAIN, RTA_GATEWAY, RTA_OIF, RTA_PRIORITY, RTM_NEWADDR, RTM_NEWLINK, RTM_NEWROUTE,
    RTN_UNICAST, RTPROT_BOOT, RtMsg,
};

use crate::{Address, Attr, Messages, Value, Writer};

/// The header of a reply in a dump.
fn reply(kind: u16, seq: u32) -> NlMsgHdr {
    NlMsgHdr {
        len: 0,
        kind,
        flags: NLM_F_MULTI,
        seq,
        pid: 11,
    }
}

/// A dump of one link, one address and one route, as `ip` would read it.
fn dump(buffer: &mut [u8]) -> usize {
    let mut writer = Writer::new(buffer);
    let link = IfInfoMsg {
        family: 0,
        kind: ARPHRD_ETHER,
        index: 2,
        flags: IFF_UP | IFF_RUNNING | IFF_BROADCAST,
        change: 0,
    };
    let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let _link = writer
        .message(
            reply(RTM_NEWLINK, 1),
            &link.to_bytes(),
            &[
                Attr::new(IFLA_IFNAME, Value::Name(b"eth0")),
                Attr::new(IFLA_ADDRESS, Value::Bytes(&mac)),
                Attr::new(IFLA_MTU, Value::U32(1500)),
            ],
        )
        .expect("room for the link");
    let address = IfAddrMsg {
        family: 2,
        prefix_len: 24,
        flags: 0,
        scope: RT_SCOPE_UNIVERSE,
        index: 2,
    };
    let _address = writer
        .message(
            reply(RTM_NEWADDR, 1),
            &address.to_bytes(),
            &[
                Attr::new(IFA_ADDRESS, Value::Address(Address::V4([10, 0, 2, 15]))),
                Attr::new(IFA_LOCAL, Value::Address(Address::V4([10, 0, 2, 15]))),
            ],
        )
        .expect("room for the address");
    let route = RtMsg {
        family: 2,
        dst_len: 0,
        src_len: 0,
        tos: 0,
        table: RT_TABLE_MAIN,
        protocol: RTPROT_BOOT,
        scope: RT_SCOPE_UNIVERSE,
        kind: RTN_UNICAST,
        flags: 0,
    };
    let _route = writer
        .message(
            reply(RTM_NEWROUTE, 1),
            &route.to_bytes(),
            &[
                Attr::new(RTA_GATEWAY, Value::Address(Address::V4([10, 0, 2, 2]))),
                Attr::new(RTA_OIF, Value::U32(2)),
                Attr::new(RTA_PRIORITY, Value::U32(100)),
            ],
        )
        .expect("room for the route");
    let _done = writer
        .done(reply(RTM_NEWROUTE, 1), 11)
        .expect("room for done");
    writer.len()
}

#[test]
fn a_dump_walks_back_to_the_messages_it_was_built_from() {
    let mut buffer = [0_u8; 512];
    let written = dump(&mut buffer);
    let kinds: Vec<u16> = Messages::new(buffer.get(..written).expect("what was written"))
        .map(|message| message.expect("well formed").header.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            RTM_NEWLINK,
            RTM_NEWADDR,
            RTM_NEWROUTE,
            ferrix_linux_abi::netlink::NLMSG_DONE
        ]
    );
}

#[test]
fn a_links_fixed_body_and_attributes_come_back_as_they_went_in() {
    let mut buffer = [0_u8; 512];
    let written = dump(&mut buffer);
    let message = Messages::new(buffer.get(..written).expect("written"))
        .next()
        .expect("one")
        .expect("well formed");
    let body = IfInfoMsg::from_bytes(message.body(IfInfoMsg::SIZE).expect("a body"))
        .expect("an ifinfomsg");
    assert_eq!(body.kind, ARPHRD_ETHER);
    assert_eq!(body.index, 2);
    assert_eq!(body.flags, IFF_UP | IFF_RUNNING | IFF_BROADCAST);
    let attributes = message.attributes(IfInfoMsg::SIZE);
    assert_eq!(
        attributes.find(IFLA_IFNAME).map(|found| found.as_name()),
        Some(b"eth0".as_slice())
    );
    assert_eq!(
        attributes.find(IFLA_MTU).and_then(|found| found.as_u32()),
        Some(1500)
    );
    assert_eq!(
        attributes.find(IFLA_ADDRESS).map(|found| found.as_bytes()),
        Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56].as_slice())
    );
}

#[test]
fn an_addresss_prefix_and_addresses_come_back_as_they_went_in() {
    let mut buffer = [0_u8; 512];
    let written = dump(&mut buffer);
    let message = Messages::new(buffer.get(..written).expect("written"))
        .nth(1)
        .expect("the second")
        .expect("well formed");
    let body =
        IfAddrMsg::from_bytes(message.body(IfAddrMsg::SIZE).expect("a body")).expect("ifaddrmsg");
    assert_eq!(body.prefix_len, 24);
    assert_eq!(body.index, 2);
    let attributes = message.attributes(IfAddrMsg::SIZE);
    assert_eq!(
        attributes
            .find(IFA_ADDRESS)
            .and_then(|found| found.as_address()),
        Some(Address::V4([10, 0, 2, 15]))
    );
    assert_eq!(
        attributes
            .find(IFA_LOCAL)
            .and_then(|found| found.as_address()),
        Some(Address::V4([10, 0, 2, 15]))
    );
}

#[test]
fn a_routes_gateway_interface_and_metric_come_back_as_they_went_in() {
    let mut buffer = [0_u8; 512];
    let written = dump(&mut buffer);
    let message = Messages::new(buffer.get(..written).expect("written"))
        .nth(2)
        .expect("the third")
        .expect("well formed");
    let body = RtMsg::from_bytes(message.body(RtMsg::SIZE).expect("a body")).expect("an rtmsg");
    assert_eq!(body.table, RT_TABLE_MAIN);
    assert_eq!(body.protocol, RTPROT_BOOT);
    assert_eq!(body.dst_len, 0, "the default route");
    let attributes = message.attributes(RtMsg::SIZE);
    assert_eq!(
        attributes
            .find(RTA_GATEWAY)
            .and_then(|found| found.as_address()),
        Some(Address::V4([10, 0, 2, 2]))
    );
    assert_eq!(
        attributes.find(RTA_OIF).and_then(|found| found.as_u32()),
        Some(2)
    );
    assert_eq!(
        attributes
            .find(RTA_PRIORITY)
            .and_then(|found| found.as_u32()),
        Some(100)
    );
}

#[test]
fn every_message_of_a_dump_is_multipart_until_the_done() {
    let mut buffer = [0_u8; 512];
    let written = dump(&mut buffer);
    for message in Messages::new(buffer.get(..written).expect("written")) {
        let header = message.expect("well formed").header;
        assert_eq!(
            header.flags & NLM_F_MULTI,
            NLM_F_MULTI,
            "a dump's messages all carry NLM_F_MULTI"
        );
    }
}

#[test]
fn a_dump_built_into_a_buffer_of_its_exact_size_walks_back_whole() {
    let mut sized = [0_u8; 512];
    let written = dump(&mut sized);
    let mut exact = Vec::from([0_u8; 512]);
    exact.truncate(written);
    let again = dump(&mut exact);
    assert_eq!(again, written);
    assert_eq!(Messages::new(&exact).count(), 4);
}
