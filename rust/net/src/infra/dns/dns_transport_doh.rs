//
// Copyright 2024 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

use crate::infra::dns::custom_resolver::{DnsQueryResult, DnsTransport};
use crate::infra::dns::dns_errors::Error;
use crate::infra::dns::dns_lookup::DnsLookupRequest;
use crate::infra::dns::dns_types::ResourceType;
use crate::infra::dns::lookup_result::LookupResult;
use crate::infra::dns::{dns_message, DnsResolver};
use crate::infra::http_client::{http2_client, AggregatingHttp2Client};
use crate::infra::tcp_ssl::DirectConnector;
use crate::infra::{dns, ConnectionParams, DnsSource};
use async_trait::async_trait;
use bytes::Bytes;
use const_str::ip_addr;
use futures_util::stream::{BoxStream, FuturesUnordered};
use http::request::Builder;
use http::uri::PathAndQuery;
use http::Method;
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

pub const CLOUDFLARE_NS: &str = "1.1.1.1";
pub const MAX_RESPONSE_SIZE: usize = 10240;
pub const KNOWN_NAMESERVERS: &[(&str, Ipv4Addr, Ipv6Addr)] = &[
    (
        CLOUDFLARE_NS,
        ip_addr!(v4, "1.1.1.1"),
        ip_addr!(v6, "2606:4700:4700::1111"),
    ),
    (
        "dns.google",
        ip_addr!(v4, "8.8.8.8"),
        ip_addr!(v6, "2001:4860:4860::8888"),
    ),
];

fn dns_resolver_for_known_ns(ipv6_enabled: bool) -> DnsResolver {
    let map: HashMap<_, _> = KNOWN_NAMESERVERS
        .iter()
        .map(|(name, ipv4, ipv6)| {
            (
                *name,
                LookupResult::new(DnsSource::Static, vec![*ipv4], vec![*ipv6]),
            )
        })
        .collect();
    let result = DnsResolver::new_from_static_map(map);
    result.set_ipv6_enabled(ipv6_enabled);
    result
}

/// DNS transport that sends queries over HTTPS
#[derive(Clone, Debug)]
pub struct DohTransport {
    http_client: AggregatingHttp2Client,
}

#[async_trait]
impl DnsTransport for DohTransport {
    type ConnectionParameters = ConnectionParams;

    fn dns_source() -> DnsSource {
        DnsSource::DnsOverHttpsLookup
    }

    async fn connect(
        connection_params: Self::ConnectionParameters,
        ipv6_enabled: bool,
    ) -> dns::Result<Self> {
        let connector = DirectConnector::new(dns_resolver_for_known_ns(ipv6_enabled));
        match http2_client(&connector, connection_params, MAX_RESPONSE_SIZE).await {
            Ok(http_client) => Ok(Self { http_client }),
            Err(error) => {
                log::error!("Failed to create HTTP2 client: {}", error);
                Err(Error::TransportFailure)
            }
        }
    }

    async fn send_queries(
        self,
        request: DnsLookupRequest,
    ) -> dns::Result<BoxStream<'static, dns::Result<DnsQueryResult>>> {
        let arc = Arc::new(self);
        let futures = match request.ipv6_enabled {
            true => vec![
                arc.clone()
                    .send_request(request.clone(), ResourceType::AAAA),
                arc.clone().send_request(request.clone(), ResourceType::A),
            ],
            false => vec![arc.clone().send_request(request.clone(), ResourceType::A)],
        };
        Ok(Box::pin(FuturesUnordered::from_iter(futures)))
    }
}

impl DohTransport {
    async fn send_request(
        self: Arc<Self>,
        request: DnsLookupRequest,
        resource_type: ResourceType,
    ) -> dns::Result<DnsQueryResult> {
        // In DoH, responses are correlated with requests via HTTP,
        // so request ID should always be 0
        // https://datatracker.ietf.org/doc/html/rfc8484#section-4.1
        let request_message =
            dns_message::create_request_with_id(0, &request.hostname, resource_type)?;
        let builder = Builder::new()
            .method(Method::POST)
            .header(http::header::ACCEPT, "application/dns-message")
            .header(http::header::CONTENT_TYPE, "application/dns-message");

        let (response_parts, response_body) = self
            .http_client
            .send_request_aggregate_response(
                PathAndQuery::from_static("/dns-query"),
                builder,
                Bytes::from(request_message),
            )
            .await
            .map_err(|_| Error::TransportFailure)?;

        if response_parts.status.as_u16() != 200 {
            return Err(Error::DohRequestBadStatus(response_parts.status.as_u16()));
        }
        let result = match resource_type {
            ResourceType::A => {
                DnsQueryResult::Left(dns_message::parse_response(&response_body, |bytes_vec| {
                    let octets: [u8; 4] = bytes_vec.try_into().unwrap();
                    Ok(Ipv4Addr::from(octets))
                })?)
            }
            ResourceType::AAAA => {
                DnsQueryResult::Right(dns_message::parse_response(&response_body, |bytes_vec| {
                    let octets: [u8; 16] = bytes_vec.try_into().unwrap();
                    Ok(Ipv6Addr::from(octets))
                })?)
            }
        };
        Ok(result)
    }
}
