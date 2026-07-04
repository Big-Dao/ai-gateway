use axum::response::IntoResponse;
use http::{header, StatusCode};

/// Serve the admin UI HTML page.
pub async fn admin_page() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("static/index.html"),
    )
}

/// Serve the admin UI CSS.
pub async fn admin_css() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("static/admin.css"),
    )
}

/// Serve the admin UI JavaScript.
pub async fn admin_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("static/admin.js"),
    )
}
