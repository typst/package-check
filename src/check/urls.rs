use codespan_reporting::diagnostic::{Diagnostic, Label};
use reqwest::StatusCode;
use typst::syntax::FileId;

use crate::check::Diagnostics;
use crate::check::manifest::{Manifest, Spanned, manifest_id};

pub async fn check(diags: &mut Diagnostics, manifest: &Manifest) {
    if let Some(homepage) = &manifest.package.homepage {
        check_url(diags, manifest_id(), homepage, "homepage").await;
    }
    if let Some(repo) = &manifest.package.repository {
        check_url(diags, manifest_id(), repo, "repository").await;
    }
}

async fn check_url(
    diags: &mut Diagnostics,
    file_id: FileId,
    field: &Spanned<String>,
    name: &'static str,
) {
    if let Err(e) = reqwest::get(&field.val)
        .await
        .and_then(|res| res.error_for_status())
    {
        let kind = if matches!(
            e.status(),
            Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        ) {
            "private"
        } else {
            "unreachable"
        };

        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(file_id, field.span()))
                .with_code(format!("manifest/package/{}/{}", name, kind))
                .with_message(format!(
                    "We could not fetch this URL.\n\nDetails: {:#?}",
                    e.without_url()
                )),
        )
    }
}
