//! This module provides a context extension `PowerBody`.
//!
//! ### Read/write body in a easier way.
//!
//! The `roa_core` provides several methods to read/write body.
//!
//! ```rust
//! use roa::{Context, Result};
//! use futures::AsyncReadExt;
//! use futures::io::BufReader;
//! use async_std::fs::File;
//!
//! async fn get(mut ctx: Context<()>) -> Result {
//!     let mut data = String::new();
//!     // implements futures::AsyncRead.
//!     ctx.req.reader().read_to_string(&mut data).await?;
//!     println!("data: {}", data);
//!
//!     // although body is empty now...
//!     let stream = ctx.req.stream();
//!     ctx.resp
//!         // echo
//!        .write_stream(stream)
//!        // write object implementing futures::AsyncRead
//!        .write_reader(File::open("assets/author.txt").await?)
//!        // write reader with specific chunk size
//!        .write_chunk(File::open("assets/author.txt").await?, 1024)
//!        // write `Bytes`
//!        .write("I am Roa.")
//!        .write(vec![127u8, 255]);
//!     Ok(())
//! }
//! ```
//!
//! These methods are useful, but they do not deal with headers and (de)serialization.
//!
//! The `PowerBody` provides more powerful methods to handle it.
//!
//! ```rust
//! use roa::{Context, Result};
//! use roa::body::{PowerBody, DispositionType::*};
//! use serde::{Serialize, Deserialize};
//! use askama::Template;
//! use async_std::fs::File;
//! use futures::io::BufReader;
//!
//! #[derive(Debug, Serialize, Deserialize, Template)]
//! #[template(path = "user.html")]
//! struct User {
//!     id: u64,
//!     name: String,
//! }
//!
//! async fn get(mut ctx: Context<()>) -> Result {
//!     // deserialize as json.
//!     let mut user: User = ctx.read_json().await?;
//!
//!     // deserialize as x-form-urlencoded.
//!     user = ctx.read_form().await?;
//!
//!     // serialize object and write it to body,
//!     // set "Content-Type"
//!     ctx.write_json(&user)?;
//!
//!     // open file and write it to body,
//!     // set "Content-Type" and "Content-Disposition"
//!     ctx.write_file("assets/welcome.html", Inline).await?;
//!
//!     // write text,
//!     // set "Content-Type"
//!     ctx.write_text("Hello, World!");
//!
//!     // write object implementing AsyncRead,
//!     // set "Content-Type"
//!     ctx.write_octet(File::open("assets/author.txt").await?);
//!
//!     // render html template, based on [askama](https://github.com/djc/askama).
//!     // set "Content-Type"
//!     ctx.render(&user)?;
//!     Ok(())
//! }
//! ```

use crate::{async_trait, http, Context, Result, State, Status};
use bytes::Bytes;
use futures::{AsyncRead, AsyncReadExt};
use lazy_static::lazy_static;
use std::fmt::Display;

#[cfg(feature = "template")]
use askama::Template;
#[cfg(feature = "file")]
mod file;
#[cfg(feature = "file")]
pub use file::DispositionType;
#[cfg(feature = "file")]
use file::{write_file, Path};
#[cfg(any(feature = "json", feature = "urlencoded"))]
use serde::de::DeserializeOwned;

use http::{header, HeaderValue, StatusCode};
#[cfg(feature = "json")]
use serde::Serialize;

/// A context extension to read/write body more simply.
#[async_trait(?Send)]
pub trait PowerBody {
    /// read request body as Bytes.
    async fn body(&mut self) -> Result<Vec<u8>>;

    /// read request body as "json".
    #[cfg(feature = "json")]
    #[cfg_attr(feature = "docs", doc(cfg(json)))]
    async fn read_json<B: DeserializeOwned>(&mut self) -> Result<B>;

    /// read request body as "urlencoded form".
    #[cfg(feature = "urlencoded")]
    #[cfg_attr(feature = "docs", doc(cfg(urlencoded)))]
    async fn read_form<B: DeserializeOwned>(&mut self) -> Result<B>;

    /// write object to response body as "application/json"
    #[cfg(feature = "json")]
    #[cfg_attr(feature = "docs", doc(cfg(json)))]
    fn write_json<B: Serialize>(&mut self, data: &B) -> Result;

    /// write object to response body as "text/html; charset=utf-8"
    #[cfg(feature = "template")]
    #[cfg_attr(feature = "docs", doc(cfg(template)))]
    fn render<B: Template>(&mut self, data: &B) -> Result;

    /// write object to response body as "text/plain"
    fn write_text<B: Into<Bytes>>(&mut self, data: B);

    /// write object to response body as "application/octet-stream"
    fn write_octet<B: 'static + AsyncRead + Unpin + Sync + Send>(&mut self, reader: B);

    /// write object to response body as extension name of file
    #[cfg(feature = "file")]
    #[cfg_attr(feature = "docs", doc(cfg(file)))]
    async fn write_file<P: 'static + AsRef<Path>>(
        &mut self,
        path: P,
        typ: DispositionType,
    ) -> Result;
}

// Static header value.
lazy_static! {
    static ref APPLICATION_JSON: HeaderValue =
        HeaderValue::from_static("application/json");
    static ref TEXT_HTML: HeaderValue =
        HeaderValue::from_static("text/html; charset=utf-8");
    static ref TEXT_PLAIN: HeaderValue = HeaderValue::from_static("text/plain");
    static ref APPLICATION_OCTET_STREM: HeaderValue =
        HeaderValue::from_static("application/octet-stream");
}

#[async_trait(?Send)]
impl<S: State> PowerBody for Context<S> {
    #[inline]
    async fn body(&mut self) -> Result<Vec<u8>> {
        let size_hint = self
            .header(header::CONTENT_LENGTH)
            .and_then(|result| result.ok())
            .and_then(|value| value.parse().ok());
        let mut data = match size_hint {
            Some(hint) => Vec::with_capacity(hint),
            None => Vec::new(),
        };
        self.req.reader().read_to_end(&mut data).await?;
        Ok(data)
    }

    #[cfg(feature = "json")]
    #[inline]
    async fn read_json<B: DeserializeOwned>(&mut self) -> Result<B> {
        let data = self.body().await?;
        serde_json::from_slice(&data).map_err(handle_invalid_body)
    }

    #[cfg(feature = "urlencoded")]
    #[inline]
    async fn read_form<B: DeserializeOwned>(&mut self) -> Result<B> {
        let data = self.body().await?;
        serde_urlencoded::from_bytes(&data).map_err(handle_invalid_body)
    }

    #[cfg(feature = "json")]
    #[inline]
    fn write_json<B: Serialize>(&mut self, data: &B) -> Result {
        self.resp.write(serde_json::to_vec(data)?);
        self.resp
            .headers
            .insert(header::CONTENT_TYPE, APPLICATION_JSON.clone());
        Ok(())
    }

    #[cfg(feature = "template")]
    #[inline]
    fn render<B: Template>(&mut self, data: &B) -> Result {
        self.resp.write(data.render()?);
        self.resp
            .headers
            .insert(header::CONTENT_TYPE, TEXT_HTML.clone());
        Ok(())
    }

    #[inline]
    fn write_text<B: Into<Bytes>>(&mut self, data: B) {
        self.resp.write(data);
        self.resp
            .headers
            .insert(header::CONTENT_TYPE, TEXT_PLAIN.clone());
    }

    #[inline]
    fn write_octet<B: 'static + AsyncRead + Unpin + Sync + Send>(&mut self, reader: B) {
        self.resp.write_reader(reader);
        self.resp
            .headers
            .insert(header::CONTENT_TYPE, APPLICATION_OCTET_STREM.clone());
    }

    #[cfg(feature = "file")]
    #[inline]
    async fn write_file<P: AsRef<Path>>(
        &mut self,
        path: P,
        typ: DispositionType,
    ) -> Result {
        write_file(self, path, typ).await
    }
}

#[allow(dead_code)]
#[inline]
fn handle_invalid_body(err: impl Display) -> Status {
    Status::new(
        StatusCode::BAD_REQUEST,
        format!("Invalid Body:\n{}", err),
        true,
    )
}

#[cfg(all(test, feature = "tcp"))]
mod tests {
    use super::PowerBody;
    use crate::http;
    use crate::tcp::Listener;
    use crate::{App, Context, Status};
    use askama::Template;
    use async_std::fs::File;
    use async_std::task::spawn;
    use http::header::CONTENT_TYPE;
    use http::StatusCode;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize)]
    struct UserDto {
        id: u64,
        name: String,
    }

    #[derive(Debug, Serialize, Hash, Eq, PartialEq, Clone, Template)]
    #[template(path = "user.html")]
    struct User<'a> {
        id: u64,
        name: &'a str,
    }

    impl PartialEq<UserDto> for User<'_> {
        fn eq(&self, other: &UserDto) -> bool {
            self.id == other.id && self.name == other.name
        }
    }

    #[allow(dead_code)]
    const USER: User = User {
        id: 0,
        name: "Hexilee",
    };

    #[cfg(feature = "json")]
    #[tokio::test]
    async fn read_json() -> Result<(), Box<dyn std::error::Error>> {
        async fn test(ctx: &mut Context<()>) -> Result<(), Status> {
            let user: UserDto = ctx.read_json().await?;
            assert_eq!(USER, user);
            Ok(())
        }
        let (addr, server) = App::new(()).end(test).run()?;
        spawn(server);

        let client = reqwest::Client::new();
        let resp = client
            .get(&format!("http://{}", addr))
            .json(&USER)
            .send()
            .await?;
        assert_eq!(StatusCode::OK, resp.status());
        Ok(())
    }

    #[cfg(feature = "urlencoded")]
    #[tokio::test]
    async fn read_form() -> Result<(), Box<dyn std::error::Error>> {
        async fn test(ctx: &mut Context<()>) -> Result<(), Status> {
            let user: UserDto = ctx.read_form().await?;
            assert_eq!(USER, user);
            Ok(())
        }
        let (addr, server) = App::new(()).end(test).run()?;
        spawn(server);

        let client = reqwest::Client::new();
        let resp = client
            .get(&format!("http://{}", addr))
            .form(&USER)
            .send()
            .await?;
        assert_eq!(StatusCode::OK, resp.status());
        Ok(())
    }

    #[cfg(feature = "template")]
    #[tokio::test]
    async fn render() -> Result<(), Box<dyn std::error::Error>> {
        async fn test(ctx: &mut Context<()>) -> Result<(), Status> {
            ctx.render(&USER)
        }
        let (addr, server) = App::new(()).end(test).run()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}", addr)).await?;
        assert_eq!(StatusCode::OK, resp.status());
        assert_eq!("text/html; charset=utf-8", resp.headers()[CONTENT_TYPE]);
        Ok(())
    }

    #[tokio::test]
    async fn write_text() -> Result<(), Box<dyn std::error::Error>> {
        async fn test(ctx: &mut Context<()>) -> Result<(), Status> {
            ctx.write_text("Hello, World!");
            Ok(())
        }
        let (addr, server) = App::new(()).end(test).run()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}", addr)).await?;
        assert_eq!(StatusCode::OK, resp.status());
        assert_eq!("text/plain", resp.headers()[CONTENT_TYPE]);
        assert_eq!("Hello, World!", resp.text().await?);
        Ok(())
    }

    #[tokio::test]
    async fn write_octet() -> Result<(), Box<dyn std::error::Error>> {
        async fn test(ctx: &mut Context<()>) -> Result<(), Status> {
            ctx.write_octet(File::open("../assets/author.txt").await?);
            Ok(())
        }
        let (addr, server) = App::new(()).end(test).run()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}", addr)).await?;
        assert_eq!(StatusCode::OK, resp.status());
        assert_eq!(
            mime::APPLICATION_OCTET_STREAM.as_ref(),
            resp.headers()[CONTENT_TYPE]
        );
        assert_eq!("Hexilee", resp.text().await?);
        Ok(())
    }
}