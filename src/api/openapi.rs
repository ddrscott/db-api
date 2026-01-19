use axum::response::Html;

pub fn openapi_spec() -> &'static str {
    include_str!("openapi.json")
}

pub fn swagger_ui() -> Html<&'static str> {
    Html(include_str!("swagger.html"))
}
