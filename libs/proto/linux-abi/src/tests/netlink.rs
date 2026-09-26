//! `netlink`: the numbers against a probe compiled from the UAPI headers, and
//! every header's fields at the offsets `offsetof` gave.

use crate::netlink::{
    self, IfAddrMsg, IfInfoMsg, NdMsg, NetlinkAddress, NlAttr, NlMsgErr, NlMsgHdr, RtMsg,
    nla_align, nlmsg_align,
};
use crate::socket::{self, AddressError};

#[test]
fn netlink_protocols_options_types_and_flags_match_the_headers() {
    for (value, expected, name) in [
        (netlink::NETLINK_ROUTE, 0, "NETLINK_ROUTE"),
        (
            netlink::NETLINK_KOBJECT_UEVENT,
            15,
            "NETLINK_KOBJECT_UEVENT",
        ),
        (netlink::SOL_NETLINK, 270, "SOL_NETLINK"),
        (netlink::NETLINK_ADD_MEMBERSHIP, 1, "NETLINK_ADD_MEMBERSHIP"),
        (
            netlink::NETLINK_DROP_MEMBERSHIP,
            2,
            "NETLINK_DROP_MEMBERSHIP",
        ),
        (netlink::NETLINK_PKTINFO, 3, "NETLINK_PKTINFO"),
        (netlink::NETLINK_CAP_ACK, 10, "NETLINK_CAP_ACK"),
        (netlink::NETLINK_EXT_ACK, 11, "NETLINK_EXT_ACK"),
        (
            netlink::NETLINK_GET_STRICT_CHK,
            12,
            "NETLINK_GET_STRICT_CHK",
        ),
    ] {
        assert_eq!(value, expected, "{name}");
    }
    for (value, expected, name) in [
        (netlink::NLMSG_NOOP, 1, "NLMSG_NOOP"),
        (netlink::NLMSG_ERROR, 2, "NLMSG_ERROR"),
        (netlink::NLMSG_DONE, 3, "NLMSG_DONE"),
        (netlink::NLMSG_OVERRUN, 4, "NLMSG_OVERRUN"),
        (netlink::NLMSG_MIN_TYPE, 16, "NLMSG_MIN_TYPE"),
        (netlink::NLM_F_REQUEST, 1, "NLM_F_REQUEST"),
        (netlink::NLM_F_MULTI, 2, "NLM_F_MULTI"),
        (netlink::NLM_F_ACK, 4, "NLM_F_ACK"),
        (netlink::NLM_F_ECHO, 8, "NLM_F_ECHO"),
        (netlink::NLM_F_DUMP_INTR, 16, "NLM_F_DUMP_INTR"),
        (netlink::NLM_F_DUMP_FILTERED, 32, "NLM_F_DUMP_FILTERED"),
        (netlink::NLM_F_ROOT, 256, "NLM_F_ROOT"),
        (netlink::NLM_F_MATCH, 512, "NLM_F_MATCH"),
        (netlink::NLM_F_ATOMIC, 1024, "NLM_F_ATOMIC"),
        (netlink::NLM_F_DUMP, 768, "NLM_F_DUMP"),
        (netlink::NLM_F_REPLACE, 256, "NLM_F_REPLACE"),
        (netlink::NLM_F_EXCL, 512, "NLM_F_EXCL"),
        (netlink::NLM_F_CREATE, 1024, "NLM_F_CREATE"),
        (netlink::NLM_F_APPEND, 2048, "NLM_F_APPEND"),
        (netlink::NLM_F_NONREC, 256, "NLM_F_NONREC"),
        (netlink::NLM_F_BULK, 512, "NLM_F_BULK"),
        (netlink::NLM_F_CAPPED, 256, "NLM_F_CAPPED"),
        (netlink::NLM_F_ACK_TLVS, 512, "NLM_F_ACK_TLVS"),
        (netlink::NLA_F_NESTED, 32768, "NLA_F_NESTED"),
        (netlink::NLA_F_NET_BYTEORDER, 16384, "NLA_F_NET_BYTEORDER"),
        (netlink::NLA_TYPE_MASK, 0x3fff, "NLA_TYPE_MASK"),
    ] {
        assert_eq!(value, expected, "{name}");
    }
    assert_eq!(netlink::NLMSG_ALIGNTO, 4, "NLMSG_ALIGNTO");
    assert_eq!(netlink::NLA_ALIGNTO, 4, "NLA_ALIGNTO and RTA_ALIGNTO");
}

#[test]
fn routing_message_and_attribute_numbers_match_the_headers() {
    for (value, expected, name) in [
        (netlink::RTM_NEWLINK, 16, "RTM_NEWLINK"),
        (netlink::RTM_DELLINK, 17, "RTM_DELLINK"),
        (netlink::RTM_GETLINK, 18, "RTM_GETLINK"),
        (netlink::RTM_SETLINK, 19, "RTM_SETLINK"),
        (netlink::RTM_NEWADDR, 20, "RTM_NEWADDR"),
        (netlink::RTM_DELADDR, 21, "RTM_DELADDR"),
        (netlink::RTM_GETADDR, 22, "RTM_GETADDR"),
        (netlink::RTM_NEWROUTE, 24, "RTM_NEWROUTE"),
        (netlink::RTM_DELROUTE, 25, "RTM_DELROUTE"),
        (netlink::RTM_GETROUTE, 26, "RTM_GETROUTE"),
        (netlink::RTM_NEWNEIGH, 28, "RTM_NEWNEIGH"),
        (netlink::RTM_DELNEIGH, 29, "RTM_DELNEIGH"),
        (netlink::RTM_GETNEIGH, 30, "RTM_GETNEIGH"),
        (netlink::IFLA_UNSPEC, 0, "IFLA_UNSPEC"),
        (netlink::IFLA_ADDRESS, 1, "IFLA_ADDRESS"),
        (netlink::IFLA_BROADCAST, 2, "IFLA_BROADCAST"),
        (netlink::IFLA_IFNAME, 3, "IFLA_IFNAME"),
        (netlink::IFLA_MTU, 4, "IFLA_MTU"),
        (netlink::IFLA_LINK, 5, "IFLA_LINK"),
        (netlink::IFLA_QDISC, 6, "IFLA_QDISC"),
        (netlink::IFLA_STATS, 7, "IFLA_STATS"),
        (netlink::IFLA_TXQLEN, 13, "IFLA_TXQLEN"),
        (netlink::IFLA_OPERSTATE, 16, "IFLA_OPERSTATE"),
        (netlink::IFLA_LINKMODE, 17, "IFLA_LINKMODE"),
        (netlink::IFLA_STATS64, 23, "IFLA_STATS64"),
        (netlink::IFLA_AF_SPEC, 26, "IFLA_AF_SPEC"),
        (netlink::IFLA_GROUP, 27, "IFLA_GROUP"),
        (netlink::IFLA_CARRIER, 33, "IFLA_CARRIER"),
        (netlink::IFA_UNSPEC, 0, "IFA_UNSPEC"),
        (netlink::IFA_ADDRESS, 1, "IFA_ADDRESS"),
        (netlink::IFA_LOCAL, 2, "IFA_LOCAL"),
        (netlink::IFA_LABEL, 3, "IFA_LABEL"),
        (netlink::IFA_BROADCAST, 4, "IFA_BROADCAST"),
        (netlink::IFA_ANYCAST, 5, "IFA_ANYCAST"),
        (netlink::IFA_CACHEINFO, 6, "IFA_CACHEINFO"),
        (netlink::IFA_MULTICAST, 7, "IFA_MULTICAST"),
        (netlink::IFA_FLAGS, 8, "IFA_FLAGS"),
        (netlink::RTA_UNSPEC, 0, "RTA_UNSPEC"),
        (netlink::RTA_DST, 1, "RTA_DST"),
        (netlink::RTA_SRC, 2, "RTA_SRC"),
        (netlink::RTA_IIF, 3, "RTA_IIF"),
        (netlink::RTA_OIF, 4, "RTA_OIF"),
        (netlink::RTA_GATEWAY, 5, "RTA_GATEWAY"),
        (netlink::RTA_PRIORITY, 6, "RTA_PRIORITY"),
        (netlink::RTA_PREFSRC, 7, "RTA_PREFSRC"),
        (netlink::RTA_METRICS, 8, "RTA_METRICS"),
        (netlink::RTA_MULTIPATH, 9, "RTA_MULTIPATH"),
        (netlink::RTA_TABLE, 15, "RTA_TABLE"),
        (netlink::NDA_UNSPEC, 0, "NDA_UNSPEC"),
        (netlink::NDA_DST, 1, "NDA_DST"),
        (netlink::NDA_LLADDR, 2, "NDA_LLADDR"),
        (netlink::NDA_CACHEINFO, 3, "NDA_CACHEINFO"),
        (netlink::NDA_PROBES, 4, "NDA_PROBES"),
        (netlink::NUD_NONE, 0, "NUD_NONE"),
        (netlink::NUD_INCOMPLETE, 1, "NUD_INCOMPLETE"),
        (netlink::NUD_REACHABLE, 2, "NUD_REACHABLE"),
        (netlink::NUD_STALE, 4, "NUD_STALE"),
        (netlink::NUD_DELAY, 8, "NUD_DELAY"),
        (netlink::NUD_PROBE, 16, "NUD_PROBE"),
        (netlink::NUD_FAILED, 32, "NUD_FAILED"),
        (netlink::NUD_NOARP, 64, "NUD_NOARP"),
        (netlink::NUD_PERMANENT, 128, "NUD_PERMANENT"),
        (netlink::ARPHRD_ETHER, 1, "ARPHRD_ETHER"),
        (netlink::ARPHRD_LOOPBACK, 772, "ARPHRD_LOOPBACK"),
    ] {
        assert_eq!(value, expected, "{name}");
    }
}

#[test]
fn route_values_groups_and_link_flags_match_the_headers() {
    for (value, expected, name) in [
        (netlink::RTN_UNSPEC, 0, "RTN_UNSPEC"),
        (netlink::RTN_UNICAST, 1, "RTN_UNICAST"),
        (netlink::RTN_LOCAL, 2, "RTN_LOCAL"),
        (netlink::RTN_BROADCAST, 3, "RTN_BROADCAST"),
        (netlink::RT_TABLE_UNSPEC, 0, "RT_TABLE_UNSPEC"),
        (netlink::RT_TABLE_MAIN, 254, "RT_TABLE_MAIN"),
        (netlink::RT_TABLE_LOCAL, 255, "RT_TABLE_LOCAL"),
        (netlink::RT_SCOPE_UNIVERSE, 0, "RT_SCOPE_UNIVERSE"),
        (netlink::RT_SCOPE_LINK, 253, "RT_SCOPE_LINK"),
        (netlink::RT_SCOPE_HOST, 254, "RT_SCOPE_HOST"),
        (netlink::RTPROT_UNSPEC, 0, "RTPROT_UNSPEC"),
        (netlink::RTPROT_KERNEL, 2, "RTPROT_KERNEL"),
        (netlink::RTPROT_BOOT, 3, "RTPROT_BOOT"),
        (netlink::RTPROT_STATIC, 4, "RTPROT_STATIC"),
        (netlink::RTPROT_RA, 9, "RTPROT_RA"),
        (netlink::RTPROT_DHCP, 16, "RTPROT_DHCP"),
    ] {
        assert_eq!(value, expected, "{name}");
    }
    for (value, expected, name) in [
        (netlink::RTMGRP_LINK, 1, "RTMGRP_LINK"),
        (netlink::RTMGRP_NEIGH, 4, "RTMGRP_NEIGH"),
        (netlink::RTMGRP_IPV4_IFADDR, 16, "RTMGRP_IPV4_IFADDR"),
        (netlink::RTMGRP_IPV4_ROUTE, 64, "RTMGRP_IPV4_ROUTE"),
        (netlink::RTMGRP_IPV6_IFADDR, 256, "RTMGRP_IPV6_IFADDR"),
        (netlink::RTMGRP_IPV6_ROUTE, 1024, "RTMGRP_IPV6_ROUTE"),
        (netlink::IFA_F_SECONDARY, 1, "IFA_F_SECONDARY"),
        (netlink::IFA_F_NODAD, 2, "IFA_F_NODAD"),
        (netlink::IFA_F_TENTATIVE, 64, "IFA_F_TENTATIVE"),
        (netlink::IFA_F_PERMANENT, 128, "IFA_F_PERMANENT"),
        (netlink::IFF_UP, 1, "IFF_UP"),
        (netlink::IFF_BROADCAST, 2, "IFF_BROADCAST"),
        (netlink::IFF_DEBUG, 4, "IFF_DEBUG"),
        (netlink::IFF_LOOPBACK, 8, "IFF_LOOPBACK"),
        (netlink::IFF_POINTOPOINT, 16, "IFF_POINTOPOINT"),
        (netlink::IFF_RUNNING, 64, "IFF_RUNNING"),
        (netlink::IFF_NOARP, 128, "IFF_NOARP"),
        (netlink::IFF_PROMISC, 256, "IFF_PROMISC"),
        (netlink::IFF_ALLMULTI, 512, "IFF_ALLMULTI"),
        (netlink::IFF_MULTICAST, 4096, "IFF_MULTICAST"),
        (netlink::IFF_LOWER_UP, 65536, "IFF_LOWER_UP"),
    ] {
        assert_eq!(value, expected, "{name}");
    }
    assert_eq!(netlink::IFNAMSIZ, 16, "IFNAMSIZ");
}

#[test]
fn a_netlink_address_is_twelve_bytes_with_the_port_at_four() {
    assert_eq!(NetlinkAddress::SIZE, 12, "sizeof(struct sockaddr_nl)");
    let address = NetlinkAddress {
        pid: 0x0403_0201,
        groups: 0x0807_0605,
    };
    let bytes = address.to_bytes();
    assert_eq!(bytes, [16, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(NetlinkAddress::parse(&bytes, 12), Ok(address));
    let mut padded = bytes;
    padded[2] = 0xff;
    assert_eq!(
        NetlinkAddress::parse(&padded, 12),
        Ok(address),
        "nl_pad is not looked at"
    );
    assert_eq!(
        NetlinkAddress::parse(&bytes, 11),
        Err(AddressError::TooShort)
    );
    assert_eq!(
        NetlinkAddress::parse(&bytes, 13),
        Err(AddressError::TooShort),
        "fewer bytes than the length claims"
    );
    let mut unix = bytes;
    unix[..2].copy_from_slice(&socket::AF_UNIX.to_le_bytes());
    assert_eq!(
        NetlinkAddress::parse(&unix, 12),
        Err(AddressError::WrongFamily)
    );
}

#[test]
fn the_message_error_and_attribute_headers_lie_where_offsetof_says() {
    let header = NlMsgHdr {
        len: 0x0403_0201,
        kind: 0x0605,
        flags: 0x0807,
        seq: 0x0c0b_0a09,
        pid: 0x100f_0e0d,
    };
    let bytes = header.to_bytes();
    assert_eq!(NlMsgHdr::SIZE, 16, "sizeof(struct nlmsghdr)");
    assert_eq!(
        bytes,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    );
    assert_eq!(NlMsgHdr::from_bytes(&bytes), Some(header));
    assert_eq!(NlMsgHdr::from_bytes(&bytes[..15]), None);

    let error = NlMsgErr {
        error: -22,
        msg: header,
    };
    let bytes = error.to_bytes();
    assert_eq!(NlMsgErr::SIZE, 20, "sizeof(struct nlmsgerr)");
    assert_eq!(&bytes[..4], &(-22_i32).to_le_bytes());
    assert_eq!(&bytes[4..], &header.to_bytes(), "msg at offset 4");
    assert_eq!(NlMsgErr::from_bytes(&bytes), Some(error));
    assert_eq!(NlMsgErr::from_bytes(&bytes[..19]), None);

    let attribute = NlAttr {
        len: 8,
        kind: netlink::IFLA_MTU | netlink::NLA_F_NESTED,
    };
    assert_eq!(NlAttr::SIZE, 4, "sizeof(struct nlattr) and struct rtattr");
    assert_eq!(attribute.to_bytes(), [8, 0, 4, 0x80]);
    assert_eq!(NlAttr::from_bytes(&attribute.to_bytes()), Some(attribute));
    assert_eq!(NlAttr::from_bytes(&[8, 0, 4]), None);
}

#[test]
fn the_link_and_address_headers_lie_where_offsetof_says() {
    let link = IfInfoMsg {
        family: 1,
        kind: 0x0403,
        index: 0x0807_0605,
        flags: 0x0c0b_0a09,
        change: 0x100f_0e0d,
    };
    assert_eq!(IfInfoMsg::SIZE, 16, "sizeof(struct ifinfomsg)");
    assert_eq!(
        link.to_bytes(),
        [1, 0, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        "ifi_type at 2, ifi_index at 4, ifi_flags at 8, ifi_change at 12"
    );
    assert_eq!(IfInfoMsg::from_bytes(&link.to_bytes()), Some(link));
    assert_eq!(IfInfoMsg::from_bytes(&link.to_bytes()[..15]), None);

    let address = IfAddrMsg {
        family: 1,
        prefix_len: 2,
        flags: 3,
        scope: 4,
        index: 0x0807_0605,
    };
    assert_eq!(IfAddrMsg::SIZE, 8, "sizeof(struct ifaddrmsg)");
    assert_eq!(address.to_bytes(), [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(IfAddrMsg::from_bytes(&address.to_bytes()), Some(address));
    assert_eq!(IfAddrMsg::from_bytes(&address.to_bytes()[..7]), None);
}

#[test]
fn the_route_and_neighbour_headers_lie_where_offsetof_says() {
    let route = RtMsg {
        family: 1,
        dst_len: 2,
        src_len: 3,
        tos: 4,
        table: 5,
        protocol: 6,
        scope: 7,
        kind: 8,
        flags: 0x0c0b_0a09,
    };
    assert_eq!(RtMsg::SIZE, 12, "sizeof(struct rtmsg)");
    assert_eq!(route.to_bytes(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    assert_eq!(RtMsg::from_bytes(&route.to_bytes()), Some(route));
    assert_eq!(RtMsg::from_bytes(&route.to_bytes()[..11]), None);

    let neighbour = NdMsg {
        family: 1,
        ifindex: 0x0807_0605,
        state: 0x0a09,
        flags: 11,
        kind: 12,
    };
    assert_eq!(NdMsg::SIZE, 12, "sizeof(struct ndmsg)");
    assert_eq!(
        neighbour.to_bytes(),
        [1, 0, 0, 0, 5, 6, 7, 8, 9, 10, 11, 12],
        "ndm_ifindex at 4, ndm_state at 8, ndm_flags at 10, ndm_type at 11"
    );
    assert_eq!(NdMsg::from_bytes(&neighbour.to_bytes()), Some(neighbour));
    assert_eq!(NdMsg::from_bytes(&neighbour.to_bytes()[..11]), None);
}

#[test]
fn alignment_rounds_up_to_four_and_saturates() {
    for (len, aligned) in [(0, 0), (1, 4), (4, 4), (5, 8), (16, 16), (17, 20)] {
        assert_eq!(nlmsg_align(len), aligned, "NLMSG_ALIGN({len})");
        assert_eq!(nla_align(len), aligned, "NLA_ALIGN({len})");
    }
    assert_eq!(nlmsg_align(usize::MAX), usize::MAX);
}
