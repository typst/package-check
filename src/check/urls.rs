use reqwest::{Response, StatusCode};
use typst::syntax::FileId;

use crate::check::Diagnostics;
use crate::check::diagnostics::{Code, Diagnostic, Label};
use crate::check::manifest::{Manifest, Spanned, manifest_id};

pub async fn check(diags: &mut Diagnostics, manifest: &Manifest) {
    // Run request in parallel.
    let mut tasks = Vec::new();

    if let Some(homepage) = &manifest.package.homepage {
        tasks.push(check_url(
            manifest_id(),
            Code::ManifestPackageHomepageUnreachable,
            homepage.clone(),
            "homepage",
        ));
    }
    if let Some(repo) = &manifest.package.repository {
        tasks.push(check_url(
            manifest_id(),
            Code::ManifestPackageRepoUnreachable,
            repo.clone(),
            "repository",
        ));
    }

    for task in tasks {
        if let Err(diag) = task.await.unwrap() {
            diags.emit(diag);
        }
    }
}

fn check_url(
    file_id: FileId,
    code: Code,
    field: Spanned<String>,
    name: &'static str,
) -> tokio::task::JoinHandle<Result<Response, Diagnostic>> {
    tokio::task::spawn(async move {
        reqwest::get(&field.val)
            .await
            .and_then(|res| res.error_for_status())
            .map_err(|err| {
                let kind = if matches!(
                    err.status(),
                    Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
                ) {
                    "private"
                } else {
                    "unreachable"
                };

                Diagnostic::error(code)
                    .with_message(format!("The `{name}` URL is {kind} ({err})",))
                    .with_label(Label::new(file_id, field.span()))
            })
    })
}
