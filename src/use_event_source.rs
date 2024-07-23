use crate::core::ConnectionReadyState;
use crate::utils::StringCodec;
use crate::{js, use_event_listener};
use default_struct_builder::DefaultBuilder;
use leptos::prelude::diagnostics::SpecialNonReactiveZone;
use leptos::prelude::wrappers::read::Signal;
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Reactive [EventSource](https://developer.mozilla.org/en-US/docs/Web/API/EventSource)
///
/// An [EventSource](https://developer.mozilla.org/en-US/docs/Web/API/EventSource) or
/// [Server-Sent-Events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events)
/// instance opens a persistent connection to an HTTP server,
/// which sends events in text/event-stream format.
///
/// ## Usage
///
/// Values are decoded via the given [`Codec`].
///
/// > To use the [`JsonCodec`], you will need to add the `"serde"` feature to your project's `Cargo.toml`.
/// > To use [`ProstCodec`], add the feature `"prost"`.
///
/// ```
/// # use leptos::prelude::*;
/// # use leptos_use::{use_event_source, UseEventSourceReturn, utils::JsonCodec};
/// # use serde::{Deserialize, Serialize};
/// #
/// #[derive(Serialize, Deserialize, Clone, PartialEq)]
/// pub struct EventSourceData {
///     pub message: String,
///     pub priority: u8,
/// }
///
/// # #[component]
/// # fn Demo() -> impl IntoView {
/// let UseEventSourceReturn {
///     ready_state, data, error, close, ..
/// } = use_event_source::<EventSourceData, JsonCodec>("https://event-source-url");
/// #
/// # view! { }
/// # }
/// ```
///
/// ### Create Your Own Custom Codec
///
/// All you need to do is to implement the [`StringCodec`] trait together with `Default` and `Clone`.
///
/// ### Named Events
///
/// You can define named events when using `use_event_source_with_options`.
///
/// ```
/// # use leptos::prelude::*;
/// # use leptos_use::{use_event_source_with_options, UseEventSourceReturn, UseEventSourceOptions, utils::FromToStringCodec};
/// #
/// # #[component]
/// # fn Demo() -> impl IntoView {
/// let UseEventSourceReturn {
///     ready_state, data, error, close, ..
/// } = use_event_source_with_options::<String, FromToStringCodec>(
///     "https://event-source-url",
///     UseEventSourceOptions::default()
///         .named_events(["notice".to_string(), "update".to_string()])
/// );
/// #
/// # view! { }
/// # }
/// ```
///
/// ### Immediate
///
/// Auto-connect (enabled by default).
///
/// This will call `open()` automatically for you, and you don't need to call it by yourself.
///
/// ### Auto-Reconnection
///
/// Reconnect on errors automatically (enabled by default).
///
/// You can control the number of reconnection attempts by setting `reconnect_limit` and the
/// interval between them by setting `reconnect_interval`.
///
/// ```
/// # use leptos::prelude::*;
/// # use leptos_use::{use_event_source_with_options, UseEventSourceReturn, UseEventSourceOptions, utils::FromToStringCodec};
/// #
/// # #[component]
/// # fn Demo() -> impl IntoView {
/// let UseEventSourceReturn {
///     ready_state, data, error, close, ..
/// } = use_event_source_with_options::<bool, FromToStringCodec>(
///     "https://event-source-url",
///     UseEventSourceOptions::default()
///         .reconnect_limit(5)         // at most 5 attempts
///         .reconnect_interval(2000)   // wait for 2 seconds between attempts
/// );
/// #
/// # view! { }
/// # }
/// ```
///
/// To disable auto-reconnection, set `reconnect_limit` to `0`.
///
/// ## Server-Side Rendering
///
/// On the server-side, `use_event_source` will always return `ready_state` as `ConnectionReadyState::Closed`,
/// `data`, `event` and `error` will always be `None`, and `open` and `close` will do nothing.
pub fn use_event_source<T, C>(
    url: &str,
) -> UseEventSourceReturn<T, C::Error, impl Fn() + Clone + 'static, impl Fn() + Clone + 'static>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    C: StringCodec<T> + Default + Send + Sync,
    C::Error: Send + Sync,
{
    use_event_source_with_options(url, UseEventSourceOptions::<T, C>::default())
}

/// Version of [`use_event_source`] that takes a `UseEventSourceOptions`. See [`use_event_source`] for how to use.
pub fn use_event_source_with_options<T, C>(
    url: &str,
    options: UseEventSourceOptions<T, C>,
) -> UseEventSourceReturn<T, C::Error, impl Fn() + Clone + 'static, impl Fn() + Clone + 'static>
where
    T: Clone + PartialEq + Send + Sync + 'static,
    C: StringCodec<T> + Default + Send + Sync,
    C::Error: Send + Sync,
{
    let UseEventSourceOptions {
        codec,
        reconnect_limit,
        reconnect_interval,
        on_failed,
        immediate,
        named_events,
        with_credentials,
        _marker,
    } = options;

    let url = url.to_owned();

    let (event, set_event) = signal(None::<SendWrapper<web_sys::Event>>);
    let (data, set_data) = signal(None::<T>);
    let (ready_state, set_ready_state) = signal(ConnectionReadyState::Closed);
    let (event_source, set_event_source) = signal(None::<SendWrapper<web_sys::EventSource>>);
    let (error, set_error) = signal(None::<UseEventSourceError<C::Error>>);

    let explicitly_closed = Arc::new(AtomicBool::new(false));
    let retried = Arc::new(AtomicU64::new(0));

    let set_data_from_string = move |data_string: Option<String>| {
        if let Some(data_string) = data_string {
            match codec.decode(data_string) {
                Ok(data) => set_data.set(Some(data)),
                Err(err) => set_error.set(Some(UseEventSourceError::Deserialize(err))),
            }
        }
    };

    let close = {
        let explicitly_closed = Arc::clone(&explicitly_closed);

        move || {
            if let Some(event_source) = event_source.get_untracked() {
                event_source.close();
                set_event_source.set(None);
                set_ready_state.set(ConnectionReadyState::Closed);
                explicitly_closed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    };

    let init = StoredValue::new(None::<Arc<dyn Fn() + Send + Sync>>);

    init.set_value(Some(Arc::new({
        let explicitly_closed = Arc::clone(&explicitly_closed);
        let retried = Arc::clone(&retried);

        move || {
            use wasm_bindgen::prelude::*;

            if explicitly_closed.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }

            let mut event_src_opts = web_sys::EventSourceInit::new();
            event_src_opts.with_credentials(with_credentials);

            let es = web_sys::EventSource::new_with_event_source_init_dict(&url, &event_src_opts)
                .unwrap_throw();

            set_ready_state.set(ConnectionReadyState::Connecting);

            set_event_source.set(Some(SendWrapper::new(es.clone())));

            let on_open = Closure::wrap(Box::new(move |_: web_sys::Event| {
                set_ready_state.set(ConnectionReadyState::Open);
                set_error.set(None);
            }) as Box<dyn FnMut(web_sys::Event)>);
            es.set_onopen(Some(on_open.as_ref().unchecked_ref()));
            on_open.forget();

            let on_error = Closure::wrap(Box::new({
                let explicitly_closed = Arc::clone(&explicitly_closed);
                let retried = Arc::clone(&retried);
                let on_failed = Arc::clone(&on_failed);
                let es = es.clone();

                move |e: web_sys::Event| {
                    set_ready_state.set(ConnectionReadyState::Closed);
                    set_error.set(Some(UseEventSourceError::Event(SendWrapper::new(e))));

                    // only reconnect if EventSource isn't reconnecting by itself
                    // this is the case when the connection is closed (readyState is 2)
                    if es.ready_state() == 2
                        && !explicitly_closed.load(std::sync::atomic::Ordering::Relaxed)
                        && reconnect_limit > 0
                    {
                        es.close();

                        let retried_value =
                            retried.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;

                        if retried_value < reconnect_limit {
                            set_timeout(
                                move || {
                                    if let Some(init) = init.get_value() {
                                        init();
                                    }
                                },
                                Duration::from_millis(reconnect_interval),
                            );
                        } else {
                            #[cfg(debug_assertions)]
                            let _z = SpecialNonReactiveZone::enter();

                            on_failed();
                        }
                    }
                }
            }) as Box<dyn FnMut(web_sys::Event)>);
            es.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            on_error.forget();

            let on_message = Closure::wrap(Box::new({
                let set_data_from_string = set_data_from_string.clone();

                move |e: web_sys::MessageEvent| {
                    set_data_from_string(e.data().as_string());
                }
            }) as Box<dyn FnMut(web_sys::MessageEvent)>);
            es.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            on_message.forget();

            for event_name in named_events.clone() {
                let set_data_from_string = set_data_from_string.clone();

                let _ = use_event_listener(
                    es.clone(),
                    leptos::ev::Custom::<leptos::ev::Event>::new(event_name),
                    move |e| {
                        set_event.set(Some(SendWrapper::new(e.clone())));
                        let data_string = js!(e["data"]).ok().and_then(|d| d.as_string());
                        set_data_from_string(data_string);
                    },
                );
            }
        }
    })));

    let open;

    #[cfg(not(feature = "ssr"))]
    {
        open = {
            let close = close.clone();
            let explicitly_closed = Arc::clone(&explicitly_closed);
            let retried = Arc::clone(&retried);

            move || {
                close();
                explicitly_closed.set(false);
                retried.set(0);
                if let Some(init) = init.get_value() {
                    init();
                }
            }
        };
    }

    #[cfg(feature = "ssr")]
    {
        open = move || {};
    }

    if immediate {
        open();
    }

    on_cleanup(close.clone());

    UseEventSourceReturn {
        event_source: event_source.into(),
        event: event.into(),
        data: data.into(),
        ready_state: ready_state.into(),
        error: error.into(),
        open,
        close,
    }
}

/// Options for [`use_event_source_with_options`].
#[derive(DefaultBuilder)]
pub struct UseEventSourceOptions<T, C>
where
    T: Send + Sync + 'static,
    C: StringCodec<T> + Send + Sync,
{
    /// Decodes from the received String to a value of type `T`.
    codec: C,

    /// Retry times. Defaults to 3.
    reconnect_limit: u64,

    /// Retry interval in ms. Defaults to 3000.
    reconnect_interval: u64,

    /// On maximum retry times reached.
    on_failed: Arc<dyn Fn() + Send + Sync>,

    /// If `true` the `EventSource` connection will immediately be opened when calling this function.
    /// If `false` you have to manually call the `open` function.
    /// Defaults to `true`.
    immediate: bool,

    /// List of named events to listen for on the `EventSource`.
    #[builder(into)]
    named_events: Vec<String>,

    /// If CORS should be set to `include` credentials. Defaults to `false`.
    with_credentials: bool,

    _marker: PhantomData<T>,
}

impl<T, C> Default for UseEventSourceOptions<T, C>
where
    C: StringCodec<T> + Default + Send + Sync,
    T: Send + Sync,
{
    fn default() -> Self {
        Self {
            codec: C::default(),
            reconnect_limit: 3,
            reconnect_interval: 3000,
            on_failed: Arc::new(|| {}),
            immediate: true,
            named_events: vec![],
            with_credentials: false,
            _marker: PhantomData,
        }
    }
}

/// Return type of [`use_event_source`].
pub struct UseEventSourceReturn<T, Err, OpenFn, CloseFn>
where
    Err: Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    OpenFn: Fn() + Clone + 'static,
    CloseFn: Fn() + Clone + 'static,
{
    /// Latest data received via the `EventSource`
    pub data: Signal<Option<T>>,

    /// The current state of the connection,
    pub ready_state: Signal<ConnectionReadyState>,

    /// The latest named event
    pub event: Signal<Option<SendWrapper<web_sys::Event>>>,

    /// The current error
    pub error: Signal<Option<UseEventSourceError<Err>>>,

    /// (Re-)Opens the `EventSource` connection
    /// If the current one is active, will close it before opening a new one.
    pub open: OpenFn,

    /// Closes the `EventSource` connection
    pub close: CloseFn,

    /// The `EventSource` instance
    pub event_source: Signal<Option<SendWrapper<web_sys::EventSource>>>,
}

#[derive(Error, Debug)]
pub enum UseEventSourceError<Err> {
    #[error("Error event: {0:?}")]
    Event(SendWrapper<web_sys::Event>),

    #[error("Error decoding value")]
    Deserialize(Err),
}
