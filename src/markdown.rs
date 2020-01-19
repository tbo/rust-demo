use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use actix_http::http::HeaderValue;
use actix_service::{Service, Transform};
use actix_web::body::{BodySize, MessageBody, ResponseBody};
use actix_web::{dev::ServiceRequest, dev::ServiceResponse, http, Error};
use bytes::{Bytes, BytesMut};
use futures::future::{ok, Ready};
use pulldown_cmark::{html, Options, Parser};

pub enum ConditionalResponse<A, I> {
    Active(A),
    Inactive(I),
}

type MarkdownResponse<B> = ConditionalResponse<MarkdownBody<B>, ResponseBody<B>>;

pub struct Transformer;

impl<S: 'static, B> Transform<S> for Transformer
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    B: MessageBody + 'static,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<MarkdownResponse<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = MarkdownTransformerMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(MarkdownTransformerMiddleware { service })
    }
}

pub struct MarkdownTransformerMiddleware<S> {
    service: S,
}

impl<S, B> Service for MarkdownTransformerMiddleware<S>
where
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    B: MessageBody,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<MarkdownResponse<B>>;
    type Error = Error;
    type Future = WrapperStream<S>;

    fn poll_ready(&mut self, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, req: ServiceRequest) -> Self::Future {
        WrapperStream {
            fut: self.service.call(req),
        }
    }
}

fn get_buffer_with_capacity(capacity: BodySize) -> BytesMut {
    use BodySize::*;
    match capacity {
        Sized(capacity) => BytesMut::with_capacity(capacity),
        Sized64(capacity) => BytesMut::with_capacity(capacity as usize),
        _ => BytesMut::new(),
    }
}

#[pin_project::pin_project]
pub struct WrapperStream<S>
where
    S: Service,
{
    #[pin]
    fut: S::Future,
}

impl<S, B> Future for WrapperStream<S>
where
    B: MessageBody,
    S: Service<Request = ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
{
    type Output = Result<ServiceResponse<MarkdownResponse<B>>, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let res = futures::ready!(self.project().fut.poll(cx));

        Poll::Ready(res.map(|mut res| {
            if res
                .headers()
                .get("content-type")
                .map(|header| header.eq(&HeaderValue::from_static("text/markdown")))
                .unwrap_or(false)
            {
                res.headers_mut().insert(
                    http::header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html"),
                );
                return res.map_body(move |_, body| {
                    let size = body.size();
                    ResponseBody::Body(MarkdownResponse::Active(MarkdownBody {
                        body,
                        buffer: get_buffer_with_capacity(size),
                    }))
                });
            }
            res.map_body(move |_, body| ResponseBody::Body(MarkdownResponse::Inactive(body)))
        }))
    }
}

pub struct MarkdownBody<B> {
    body: ResponseBody<B>,
    buffer: BytesMut,
}

impl<B: MessageBody> MarkdownBody<B> {
    fn is_complete(&self) -> bool {
        use BodySize::*;
        match self.body.size() {
            None | Empty => true,
            Sized(size) => size <= self.buffer.len(),
            Sized64(size) => size <= self.buffer.len() as u64,
            _ => false,
        }
    }
}

impl<B: MessageBody> MessageBody for MarkdownResponse<B> {
    fn size(&self) -> BodySize {
        use ConditionalResponse::*;
        match self {
            Active(_body) => BodySize::Stream,
            Inactive(body) => body.size(),
        }
    }

    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, Error>>> {
        use ConditionalResponse::*;
        match self {
            Inactive(body) => body.poll_next(cx),
            Active(response_body) => match response_body.body.poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    response_body.buffer.extend_from_slice(&chunk);
                    if !response_body.is_complete() {
                        cx.waker().clone().wake();
                        return Poll::Pending;
                    }
                    let s = &String::from_utf8_lossy(&response_body.buffer);
                    let parser = Parser::new_ext(s, Options::empty());
                    let mut html_output: String = String::with_capacity(s.len() * 3 / 2);
                    html_output.push_str(
                        "<!DOCTYPE html><html><head><title>Markdown Page</title></head><body>",
                    );
                    html::push_html(&mut html_output, parser);
                    html_output.push_str("</body></html>");
                    Poll::Ready(Some(Ok(Bytes::from(html_output))))
                }
                Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}
