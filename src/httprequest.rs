//! HTTP Request message related code.
use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;
use std::{cmp, fmt, io, str};

use bytes::Bytes;
use cookie::Cookie;
use failure;
use futures::{Async, Poll, Stream};
use futures_cpupool::CpuPool;
use http::{header, HeaderMap, Method, StatusCode, Uri, Version};
use tokio_io::AsyncRead;
use url::{form_urlencoded, Url};

use body::Body;
use error::{CookieParseError, PayloadError, UrlGenerationError};
use extensions::Extensions;
use handler::FromRequest;
use httpmessage::HttpMessage;
use httpresponse::{HttpResponse, HttpResponseBuilder};
use info::ConnectionInfo;
use param::Params;
use payload::Payload;
use router::{Resource, Router};
use server::message::{MessageFlags, RequestContext};
use state::RequestState;
use uri::Url as InnerUrl;

struct Query(HashMap<String, String>);
struct Cookies(Vec<Cookie<'static>>);
struct Info(ConnectionInfo);

/// An HTTP Request
pub struct HttpRequest<S = ()> {
    msg: Rc<RequestContext>,
    state: RequestState<S>,
}

impl<S> HttpMessage for HttpRequest<S> {
    type Stream = Payload;

    #[inline]
    fn headers(&self) -> &HeaderMap {
        &self.msg.inner.headers
    }

    #[inline]
    fn payload(&self) -> Payload {
        if let Some(payload) = self.msg.inner.payload.borrow_mut().take() {
            payload
        } else {
            Payload::empty()
        }
    }
}

impl<S> HttpRequest<S> {
    #[inline]
    pub(crate) fn from_state(
        msg: RequestContext, state: RequestState<S>,
    ) -> HttpRequest<S> {
        HttpRequest {
            state,
            msg: Rc::new(msg),
        }
    }

    pub(crate) fn into_parts(self) -> (RequestContext, RequestState<S>) {
        unimplemented!()
    }

    pub(crate) fn copy_context(&self) -> RequestContext {
        unimplemented!()
    }

    pub(crate) fn as_state(&self) -> &RequestState<S> {
        &self.state
    }

    pub(crate) fn as_context(&self) -> &RequestContext {
        self.msg.as_ref()
    }

    /// Shared application state
    #[inline]
    pub fn state(&self) -> &S {
        &self.state.state
    }

    /// Request extensions
    #[inline]
    pub fn extensions(&self) -> &Extensions {
        &self.msg.inner.extensions
    }

    /// Default `CpuPool`
    #[inline]
    #[doc(hidden)]
    pub fn cpu_pool(&self) -> &CpuPool {
        self.msg.server_settings().cpu_pool()
    }

    /// Create http response
    pub fn response(&self, status: StatusCode, body: Body) -> HttpResponse {
        self.msg.server_settings().get_response(status, body)
    }

    /// Create http response builder
    pub fn build_response(&self, status: StatusCode) -> HttpResponseBuilder {
        self.msg.server_settings().get_response_builder(status)
    }

    /// Read the Request Uri.
    #[inline]
    pub fn uri(&self) -> &Uri {
        self.msg.inner.url.uri()
    }

    /// Read the Request method.
    #[inline]
    pub fn method(&self) -> &Method {
        &self.msg.inner.method
    }

    /// Read the Request Version.
    #[inline]
    pub fn version(&self) -> Version {
        self.msg.inner.version
    }

    /// The target path of this Request.
    #[inline]
    pub fn path(&self) -> &str {
        self.msg.inner.url.path()
    }

    #[inline]
    pub(crate) fn url(&self) -> &InnerUrl {
        &self.msg.inner.url
    }

    /// Get *ConnectionInfo* for the correct request.
    #[inline]
    pub fn connection_info(&self) -> Ref<ConnectionInfo> {
        self.msg.connection_info()
    }

    /// Generate url for named resource
    ///
    /// ```rust
    /// # extern crate actix_web;
    /// # use actix_web::{App, HttpRequest, HttpResponse, http};
    /// #
    /// fn index(req: HttpRequest) -> HttpResponse {
    ///     let url = req.url_for("foo", &["1", "2", "3"]); // <- generate url for "foo" resource
    ///     HttpResponse::Ok().into()
    /// }
    ///
    /// fn main() {
    ///     let app = App::new()
    ///         .resource("/test/{one}/{two}/{three}", |r| {
    ///              r.name("foo");  // <- set resource name, then it could be used in `url_for`
    ///              r.method(http::Method::GET).f(|_| HttpResponse::Ok());
    ///         })
    ///         .finish();
    /// }
    /// ```
    pub fn url_for<U, I>(
        &self, name: &str, elements: U,
    ) -> Result<Url, UrlGenerationError>
    where
        U: IntoIterator<Item = I>,
        I: AsRef<str>,
    {
        let path = self.router().resource_path(name, elements)?;
        if path.starts_with('/') {
            let conn = self.connection_info();
            Ok(Url::parse(&format!(
                "{}://{}{}",
                conn.scheme(),
                conn.host(),
                path
            ))?)
        } else {
            Ok(Url::parse(&path)?)
        }
    }

    /// Generate url for named resource
    ///
    /// This method is similar to `HttpRequest::url_for()` but it can be used
    /// for urls that do not contain variable parts.
    pub fn url_for_static(&self, name: &str) -> Result<Url, UrlGenerationError> {
        const NO_PARAMS: [&str; 0] = [];
        self.url_for(name, &NO_PARAMS)
    }

    /// This method returns reference to current `Router` object.
    #[inline]
    pub fn router(&self) -> &Router {
        &self.state.router
    }

    /// This method returns reference to matched `Resource` object.
    #[inline]
    pub fn resource(&self) -> Option<&Resource> {
        self.state.resource()
    }

    /// Peer socket address
    ///
    /// Peer address is actual socket address, if proxy is used in front of
    /// actix http server, then peer address would be address of this proxy.
    ///
    /// To get client connection information `connection_info()` method should
    /// be used.
    #[inline]
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.msg.inner.addr
    }

    /// url query parameters.
    pub fn query(&self) -> &HashMap<String, String> {
        unimplemented!()
        /*
        if self.extensions().get::<Query>().is_none() {
            let mut query = HashMap::new();
            for (key, val) in form_urlencoded::parse(self.query_string().as_ref()) {
                query.insert(key.as_ref().to_string(), val.to_string());
            }
            let mut req = self.clone();
            req.as_mut().extensions.insert(Query(query));
        }
        &self.extensions().get::<Query>().unwrap().0
         */
    }

    /// The query string in the URL.
    ///
    /// E.g., id=10
    #[inline]
    pub fn query_string(&self) -> &str {
        if let Some(query) = self.uri().query().as_ref() {
            query
        } else {
            ""
        }
    }

    /// Load request cookies.
    pub fn cookies(&self) -> Result<&Vec<Cookie<'static>>, CookieParseError> {
        unimplemented!()
        /*
        if self.extensions().get::<Query>().is_none() {
            let mut req = self.clone();
            let msg = req.as_mut();
            let mut cookies = Vec::new();
            for hdr in msg.headers.get_all(header::COOKIE) {
                let s = str::from_utf8(hdr.as_bytes()).map_err(CookieParseError::from)?;
                for cookie_str in s.split(';').map(|s| s.trim()) {
                    if !cookie_str.is_empty() {
                        cookies.push(Cookie::parse_encoded(cookie_str)?.into_owned());
                    }
                }
            }
            msg.extensions.insert(Cookies(cookies));
        }
        Ok(&self.extensions().get::<Cookies>().unwrap().0)*/
    }

    /// Return request cookie.
    pub fn cookie(&self, name: &str) -> Option<&Cookie> {
        if let Ok(cookies) = self.cookies() {
            for cookie in cookies {
                if cookie.name() == name {
                    return Some(cookie);
                }
            }
        }
        None
    }

    pub(crate) fn set_cookies(&mut self, cookies: Option<Vec<Cookie<'static>>>) {
        //if let Some(cookies) = cookies {
        //self.extensions_mut().insert(Cookies(cookies));
        //}
    }

    /// Get a reference to the Params object.
    ///
    /// Params is a container for url parameters.
    /// A variable segment is specified in the form `{identifier}`,
    /// where the identifier can be used later in a request handler to
    /// access the matched value for that segment.
    #[inline]
    pub fn match_info(&self) -> &Params {
        &self.msg.inner.params
    }

    /// Check if request requires connection upgrade
    pub(crate) fn upgrade(&self) -> bool {
        self.msg.upgrade()
    }

    /// Set read buffer capacity
    ///
    /// Default buffer capacity is 32Kb.
    pub fn set_read_buffer_capacity(&mut self, cap: usize) {
        if let Some(payload) = self.msg.inner.payload.borrow_mut().as_mut() {
            payload.set_read_buffer_capacity(cap)
        }
    }

    /*
    #[cfg(test)]
    pub(crate) fn payload(&mut self) -> Ref<Payload> {
        if self.msg.inner.payload.is_none() {
            *self.msg.inner.payload.borrow_mut() = Some(Payload::empty());
        }
        self.msg.inner.payload.borrow()
    }

    #[cfg(test)]
    pub(crate) fn payload_mut(&mut self) -> &mut Payload {
        let msg = self.as_mut();
        if msg.payload.is_none() {
            msg.payload = Some(Payload::empty());
        }
        msg.payload.as_mut().unwrap()
    }
    */
}

impl<S> Clone for HttpRequest<S> {
    fn clone(&self) -> HttpRequest<S> {
        HttpRequest {
            msg: self.msg.clone(),
            state: self.state.clone(),
        }
    }
}

impl<S> FromRequest<S> for HttpRequest<S> {
    type Config = ();
    type Result = Self;

    #[inline]
    fn from_request(req: &HttpRequest<S>, _: &Self::Config) -> Self::Result {
        req.clone()
    }
}

impl<S> fmt::Debug for HttpRequest<S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let res = writeln!(
            f,
            "\nHttpRequest {:?} {}:{}",
            self.version(),
            self.method(),
            self.path()
        );
        if !self.query_string().is_empty() {
            let _ = writeln!(f, "  query: ?{:?}", self.query_string());
        }
        if !self.match_info().is_empty() {
            let _ = writeln!(f, "  params: {:?}", self.match_info());
        }
        let _ = writeln!(f, "  headers:");
        for (key, val) in self.headers().iter() {
            let _ = writeln!(f, "    {:?}: {:?}", key, val);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use resource::ResourceHandler;
    use router::Resource;
    use test::TestRequest;

    #[test]
    fn test_debug() {
        let req = TestRequest::with_header("content-type", "text/plain").finish();
        let dbg = format!("{:?}", req);
        assert!(dbg.contains("HttpRequest"));
    }

    #[test]
    fn test_no_request_cookies() {
        let req = TestRequest::default().finish();
        assert!(req.cookies().unwrap().is_empty());
    }

    #[test]
    fn test_request_cookies() {
        let req = TestRequest::default()
            .header(header::COOKIE, "cookie1=value1")
            .header(header::COOKIE, "cookie2=value2")
            .finish();
        {
            let cookies = req.cookies().unwrap();
            assert_eq!(cookies.len(), 2);
            assert_eq!(cookies[0].name(), "cookie1");
            assert_eq!(cookies[0].value(), "value1");
            assert_eq!(cookies[1].name(), "cookie2");
            assert_eq!(cookies[1].value(), "value2");
        }

        let cookie = req.cookie("cookie1");
        assert!(cookie.is_some());
        let cookie = cookie.unwrap();
        assert_eq!(cookie.name(), "cookie1");
        assert_eq!(cookie.value(), "value1");

        let cookie = req.cookie("cookie-unknown");
        assert!(cookie.is_none());
    }

    #[test]
    fn test_request_query() {
        let req = TestRequest::with_uri("/?id=test").finish();
        assert_eq!(req.query_string(), "id=test");
        let query = req.query();
        assert_eq!(&query["id"], "test");
    }

    #[test]
    fn test_request_match_info() {
        let mut resource = ResourceHandler::<()>::default();
        resource.name("index");
        let mut routes = Vec::new();
        routes.push((Resource::new("index", "/{key}/"), Some(resource)));
        let (router, _) = Router::new("", routes);

        let (mut ctx, mut state) = TestRequest::with_uri("/value/?id=test").context();
        assert!(router.recognize(&mut ctx, &mut state).is_some());
        assert_eq!(ctx.match_info().get("key"), Some("value"));
    }

    #[test]
    fn test_url_for() {
        let mut resource = ResourceHandler::<()>::default();
        resource.name("index");
        let routes =
            vec![(Resource::new("index", "/user/{name}.{ext}"), Some(resource))];
        let (router, _) = Router::new("/", routes);
        assert!(router.has_route("/user/test.html"));
        assert!(!router.has_route("/test/unknown"));

        let req = TestRequest::with_header(header::HOST, "www.rust-lang.org")
            .finish_with_router(router);

        assert_eq!(
            req.url_for("unknown", &["test"]),
            Err(UrlGenerationError::ResourceNotFound)
        );
        assert_eq!(
            req.url_for("index", &["test"]),
            Err(UrlGenerationError::NotEnoughElements)
        );
        let url = req.url_for("index", &["test", "html"]);
        assert_eq!(
            url.ok().unwrap().as_str(),
            "http://www.rust-lang.org/user/test.html"
        );
    }

    #[test]
    fn test_url_for_with_prefix() {
        let mut resource = ResourceHandler::<()>::default();
        resource.name("index");
        let routes = vec![(Resource::new("index", "/user/{name}.html"), Some(resource))];
        let (router, _) = Router::new("/prefix/", routes);
        assert!(router.has_route("/user/test.html"));
        assert!(!router.has_route("/prefix/user/test.html"));

        let req = TestRequest::with_header(header::HOST, "www.rust-lang.org")
            .finish_with_router(router);
        let url = req.url_for("index", &["test"]);
        assert_eq!(
            url.ok().unwrap().as_str(),
            "http://www.rust-lang.org/prefix/user/test.html"
        );
    }

    #[test]
    fn test_url_for_static() {
        let mut resource = ResourceHandler::<()>::default();
        resource.name("index");
        let routes = vec![(Resource::new("index", "/index.html"), Some(resource))];
        let (router, _) = Router::new("/prefix/", routes);
        assert!(router.has_route("/index.html"));
        assert!(!router.has_route("/prefix/index.html"));

        let req = TestRequest::default()
            .header(header::HOST, "www.rust-lang.org")
            .finish_with_router(router);
        let url = req.url_for_static("index");
        assert_eq!(
            url.ok().unwrap().as_str(),
            "http://www.rust-lang.org/prefix/index.html"
        );
    }

    #[test]
    fn test_url_for_external() {
        let mut resource = ResourceHandler::<()>::default();
        resource.name("index");
        let routes = vec![(
            Resource::external("youtube", "https://youtube.com/watch/{video_id}"),
            None,
        )];
        let (router, _) = Router::new::<()>("", routes);
        assert!(!router.has_route("https://youtube.com/watch/unknown"));

        let req = TestRequest::default().finish_with_router(router);
        let url = req.url_for("youtube", &["oHg5SJYRHA0"]);
        assert_eq!(
            url.ok().unwrap().as_str(),
            "https://youtube.com/watch/oHg5SJYRHA0"
        );
    }
}
