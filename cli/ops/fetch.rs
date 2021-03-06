// Copyright 2018-2020 the Deno authors. All rights reserved. MIT license.
use super::dispatch_json::{Deserialize, JsonOp, Value};
use super::io::{StreamResource, StreamResourceHolder};
use crate::http_util::{create_http_client, HttpBody};
use crate::state::State;
use deno_core::CoreIsolate;
use deno_core::CoreIsolateState;
use deno_core::ErrBox;
use deno_core::ZeroCopyBuf;
use futures::future::FutureExt;
use http::header::HeaderName;
use http::header::HeaderValue;
use http::Method;
use reqwest::Client;
use std::convert::From;
use std::path::PathBuf;
use std::rc::Rc;

pub fn init(i: &mut CoreIsolate, s: &Rc<State>) {
  i.register_op("op_fetch", s.stateful_json_op2(op_fetch));
  i.register_op(
    "op_create_http_client",
    s.stateful_json_op2(op_create_http_client),
  );
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchArgs {
  method: Option<String>,
  url: String,
  headers: Vec<(String, String)>,
  client_rid: Option<u32>,
}

pub fn op_fetch(
  isolate_state: &mut CoreIsolateState,
  state: &Rc<State>,
  args: Value,
  data: &mut [ZeroCopyBuf],
) -> Result<JsonOp, ErrBox> {
  let args: FetchArgs = serde_json::from_value(args)?;
  let url = args.url;
  let resource_table_ = isolate_state.resource_table.borrow();

  let mut client_ref_mut;
  let client = if let Some(rid) = args.client_rid {
    let r = resource_table_
      .get::<HttpClientResource>(rid)
      .ok_or_else(ErrBox::bad_resource_id)?;
    &r.client
  } else {
    client_ref_mut = state.http_client.borrow_mut();
    &mut *client_ref_mut
  };

  let method = match args.method {
    Some(method_str) => Method::from_bytes(method_str.as_bytes())?,
    None => Method::GET,
  };

  let url_ = url::Url::parse(&url)?;

  // Check scheme before asking for net permission
  let scheme = url_.scheme();
  if scheme != "http" && scheme != "https" {
    return Err(ErrBox::type_error(format!(
      "scheme '{}' not supported",
      scheme
    )));
  }

  state.check_net_url(&url_)?;

  let mut request = client.request(method, url_);

  match data.len() {
    0 => {}
    1 => request = request.body(Vec::from(&*data[0])),
    _ => panic!("Invalid number of arguments"),
  }

  for (key, value) in args.headers {
    let name = HeaderName::from_bytes(key.as_bytes()).unwrap();
    let v = HeaderValue::from_str(&value).unwrap();
    request = request.header(name, v);
  }
  debug!("Before fetch {}", url);

  let resource_table = isolate_state.resource_table.clone();
  let future = async move {
    let res = request.send().await?;
    debug!("Fetch response {}", url);
    let status = res.status();
    let mut res_headers = Vec::new();
    for (key, val) in res.headers().iter() {
      res_headers.push((key.to_string(), val.to_str().unwrap().to_owned()));
    }

    let body = HttpBody::from(res);
    let mut resource_table = resource_table.borrow_mut();
    let rid = resource_table.add(
      "httpBody",
      Box::new(StreamResourceHolder::new(StreamResource::HttpBody(
        Box::new(body),
      ))),
    );

    let json_res = json!({
      "bodyRid": rid,
      "status": status.as_u16(),
      "statusText": status.canonical_reason().unwrap_or(""),
      "headers": res_headers
    });

    Ok(json_res)
  };

  Ok(JsonOp::Async(future.boxed_local()))
}

struct HttpClientResource {
  client: Client,
}

impl HttpClientResource {
  fn new(client: Client) -> Self {
    Self { client }
  }
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct CreateHttpClientOptions {
  ca_file: Option<String>,
}

fn op_create_http_client(
  isolate_state: &mut CoreIsolateState,
  state: &Rc<State>,
  args: Value,
  _zero_copy: &mut [ZeroCopyBuf],
) -> Result<JsonOp, ErrBox> {
  let args: CreateHttpClientOptions = serde_json::from_value(args)?;
  let mut resource_table = isolate_state.resource_table.borrow_mut();

  if let Some(ca_file) = args.ca_file.clone() {
    state.check_read(&PathBuf::from(ca_file))?;
  }

  let client = create_http_client(args.ca_file.as_deref()).unwrap();

  let rid =
    resource_table.add("httpClient", Box::new(HttpClientResource::new(client)));
  Ok(JsonOp::Sync(json!(rid)))
}
