use crate::{
    default_status_handler, first_middleware, last, Context, DynHandler, DynMiddleware,
    DynStatusHandler, Middleware, Model, Next, Request, Response, Status, StatusHandler,
    TargetHandler,
};

use async_std::net::{TcpListener, ToSocketAddrs};
use async_std::task;
use http_service::HttpService;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub struct Builder<M: Model = ()> {
    middleware: Arc<DynMiddleware<M::State>>,
    status_handler: Box<DynStatusHandler<M::State>>,
}

pub struct Service<M: Model> {
    handler: Arc<DynHandler<M::State>>,
    status_handler: Arc<DynStatusHandler<M::State>>,
    pub(crate) model: Arc<M>,
}

impl<M: Model> Builder<M> {
    pub fn new() -> Self {
        Self {
            middleware: Arc::from(Box::new(first_middleware).dynamic()),
            status_handler: Box::new(default_status_handler).dynamic(),
        }
    }

    pub fn handle_fn<F>(
        self,
        middleware: impl 'static + Sync + Send + Fn(Context<M::State>, Next) -> F,
    ) -> Self
    where
        F: 'static + Future<Output = Result<(), Status>> + Send,
    {
        self.handle(middleware)
    }

    pub fn handle<F>(self, middleware: impl Middleware<M::State, StatusFuture = F>) -> Self
    where
        F: 'static + Future<Output = Result<(), Status>> + Send,
    {
        let current_middleware = self.middleware.clone();
        let next_middleware: Arc<DynMiddleware<M::State>> =
            Arc::from(Box::new(middleware).dynamic());
        Self {
            middleware: Arc::from(move |ctx: Context<M::State>, next| {
                let next_ref = next_middleware.clone();
                let ctx_cloned = ctx.clone();
                let current = Box::new(move || next_ref(ctx_cloned, next));
                current_middleware(ctx, current)
            }),
            ..self
        }
    }

    pub fn handle_status<F>(self, handler: impl StatusHandler<M::State, StatusFuture = F>) -> Self
    where
        F: 'static + Future<Output = Result<(), Status>> + Send,
    {
        Self {
            status_handler: Box::new(handler).dynamic(),
            ..self
        }
    }

    pub fn handle_status_fn<F>(
        self,
        handler: impl 'static + Sync + Send + Fn(Context<M::State>, Status) -> F,
    ) -> Self
    where
        F: 'static + Future<Output = Result<(), Status>> + Send,
    {
        self.handle_status(handler)
    }

    pub fn model(self, model: M) -> Service<M> {
        let Builder {
            middleware,
            status_handler,
        } = self;
        Service::new(
            Arc::new(move |ctx| middleware(ctx, Box::new(last))),
            Arc::from(status_handler),
            Arc::new(model),
        )
    }
}

impl<M: Model> Default for Builder<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Model> Service<M> {
    pub fn builder() -> Builder<M> {
        Builder::default()
    }

    pub fn new(
        handler: Arc<DynHandler<M::State>>,
        status_handler: Arc<DynStatusHandler<M::State>>,
        model: Arc<M>,
    ) -> Self {
        Self {
            handler,
            status_handler,
            model,
        }
    }

    pub async fn serve(&self, req: http_service::Request) -> Result<Response, Status> {
        let mut context = Context::new(Request::new(req), self.clone());
        let app = self.clone();
        if let Err(status) = (app.handler)(context.clone()).await {
            (app.status_handler)(context.clone(), status).await?;
        }
        Ok(std::mem::take(&mut context.response))
    }

    pub async fn listen(&self, addr: impl ToSocketAddrs) -> Result<(), std::io::Error> {
        let http_service = self.clone();
        #[derive(Copy, Clone)]
        struct Spawner;

        impl futures::task::Spawn for &Spawner {
            fn spawn_obj(
                &self,
                future: futures::future::FutureObj<'static, ()>,
            ) -> Result<(), futures::task::SpawnError> {
                task::spawn(Box::pin(future));
                Ok(())
            }
        }

        let listener = TcpListener::bind(addr).await?;
        log::info!("Server is listening on: http://{}", listener.local_addr()?);
        http_service_hyper::Server::builder(listener.incoming())
            .with_spawner(Spawner {})
            .serve(http_service)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }
}

impl<M: Model> HttpService for Service<M> {
    type Connection = ();
    type ConnectionFuture =
        Pin<Box<dyn 'static + Future<Output = Result<(), Infallible>> + Sync + Send>>;
    fn connect(&self) -> Self::ConnectionFuture {
        Box::pin(async { Ok(()) })
    }

    type ResponseFuture =
        Pin<Box<dyn 'static + Future<Output = Result<http_service::Response, Status>> + Send>>;

    fn respond(&self, _conn: &mut (), req: http_service::Request) -> Self::ResponseFuture {
        let service = self.clone();
        Box::pin(async move { Ok(service.serve(req).await?.into_resp()?) })
    }
}

impl<M: Model> Clone for Service<M> {
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            status_handler: self.status_handler.clone(),
            model: self.model.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Service;
    use futures::lock::Mutex;
    use http_service::{Body, Request};
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::time::Instant;

    #[async_std::test]
    async fn gate_simple() -> Result<(), Infallible> {
        let _resp = Service::builder()
            .handle_fn(|_ctx, next| {
                async move {
                    let inbound = Instant::now();
                    next().await?;
                    println!("time elapsed: {} ms", inbound.elapsed().as_millis());
                    Ok(())
                }
            })
            .model(())
            .serve(Request::new(Body::empty()))
            .await;
        Ok(())
    }

    #[async_std::test]
    async fn middleware_order() -> Result<(), Infallible> {
        let vector = Arc::new(Mutex::new(Vec::new()));
        let mut builder = Service::builder();
        for i in 0..100 {
            let vec = vector.clone();
            builder = builder.handle_fn(move |_ctx, next| {
                let vec = vec.clone();
                async move {
                    vec.lock().await.push(i);
                    next().await?;
                    vec.lock().await.push(i);
                    Ok(())
                }
            });
        }
        let _resp = builder.model(()).serve(Request::new(Body::empty())).await;
        for i in 0..100 {
            assert_eq!(i, vector.lock().await[i]);
            assert_eq!(i, vector.lock().await[199 - i]);
        }
        Ok(())
    }
}