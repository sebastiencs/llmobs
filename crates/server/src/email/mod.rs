//! Outgoing mail: SMTP through lettre, minijinja templates, background retries.
//!
//! Logs name the template, the message id, and the attempt. They never include
//! the recipient, the subject, or the body (spec 08 § Email, D150).

#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use lettre::message::{Mailbox, Message, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use llmobs_core::names::PRODUCT;
use minijinja::{AutoEscape, Environment, Value, context};

use crate::api::account::OrgRole;
use crate::config::{ServerConfig, SmtpUrl, Tls};

/// How many times [`Mailer::send_in_background`] tries, including the first.
const ATTEMPTS: u32 = 3;

const TEMPLATES: &[(&str, &str)] = &[
    ("invite.txt", include_str!("templates/invite.txt")),
    ("invite.html", include_str!("templates/invite.html")),
    (
        "password_reset.txt",
        include_str!("templates/password_reset.txt"),
    ),
    (
        "password_reset.html",
        include_str!("templates/password_reset.html"),
    ),
    (
        "verify_email.txt",
        include_str!("templates/verify_email.txt"),
    ),
    (
        "verify_email.html",
        include_str!("templates/verify_email.html"),
    ),
    ("test.txt", include_str!("templates/test.txt")),
    ("test.html", include_str!("templates/test.html")),
];

/// One transactional message. The `url` is the link the later phase built.
#[allow(clippy::enum_variant_names)] // `VerifyEmail` is the template name.
pub(crate) enum Email {
    /// Invitation to an org.
    Invite {
        /// Org display name.
        org_name: String,
        /// The person who invited, when the caller knows one.
        inviter_name: Option<String>,
        /// Role the invite grants.
        role: OrgRole,
        /// `<public_url>/invite/<token>`.
        url: String,
    },
    /// Password reset.
    PasswordReset {
        /// `<public_url>/reset?token=…`.
        url: String,
    },
    /// Address verification.
    VerifyEmail {
        /// `<public_url>/verify?token=…`.
        url: String,
    },
    /// Instance-settings test message.
    Test,
}

/// Subject, plain text, and HTML for one [`Email`].
pub(crate) struct Rendered {
    /// Subject line, built from [`PRODUCT`].
    pub(crate) subject: String,
    /// Plain-text part.
    pub(crate) text: String,
    /// HTML part. `.html` templates are auto-escaped.
    pub(crate) html: String,
}

/// A message the stub transport accepted.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SentMail {
    /// Envelope recipient.
    pub(crate) to: String,
    /// Subject line.
    pub(crate) subject: String,
    /// Plain-text part.
    pub(crate) text: String,
    /// HTML part.
    pub(crate) html: String,
}

#[cfg(test)]
struct StubInbox {
    sent: Vec<SentMail>,
    failures_left: usize,
}

#[cfg(test)]
#[derive(Debug)]
struct StubFailure;

#[cfg(test)]
impl fmt::Display for StubFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("smtp connection failed")
    }
}

#[cfg(test)]
impl std::error::Error for StubFailure {}

/// Why a message was not sent.
///
/// [`Display`] and [`Debug`] are fixed strings. A lettre error is kept only as
/// [`std::error::Error::source`] and is never written into a response or a log:
/// the server reply can quote the recipient.
#[derive(thiserror::Error)]
pub(crate) enum MailError {
    /// The connection could not be opened.
    #[error("could not connect to the smtp server")]
    Connect(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The TLS handshake failed.
    #[error("smtp tls failed")]
    Tls(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The server rejected the credentials.
    #[error("smtp authentication failed")]
    Auth(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The server refused the message.
    #[error("the smtp server rejected the message")]
    Rejected(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A template did not render.
    #[error("the email template failed")]
    Template(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// `From` or `To` is not a mailbox.
    #[error("invalid email address")]
    InvalidAddress,
}

impl fmt::Debug for MailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.kind())
    }
}

impl MailError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Connect(_) => "connect",
            Self::Tls(_) => "tls",
            Self::Auth(_) => "auth",
            Self::Rejected(_) => "rejected",
            Self::Template(_) => "template",
            Self::InvalidAddress => "invalid_address",
        }
    }
}

struct Failure {
    message_id: String,
    error: MailError,
}

enum Transport {
    Smtp(AsyncSmtpTransport<Tokio1Executor>),
    #[cfg(test)]
    Stub(Arc<std::sync::Mutex<StubInbox>>),
}

/// SMTP client, the `From` mailbox, and the embedded templates.
pub(crate) struct Mailer {
    transport: Transport,
    from: Mailbox,
    templates: Environment<'static>,
}

impl Mailer {
    /// `None` when `smtp_url` is unset. Does not connect.
    ///
    /// # Errors
    ///
    /// Returns [`MailError`] when the host cannot be used as an SMTP server
    /// name or a template does not parse. The message does not include the URL.
    pub(crate) fn from_config(config: &ServerConfig) -> Result<Option<Self>, MailError> {
        let Some(smtp) = &config.smtp_url else {
            return Ok(None);
        };
        let from = mailbox(
            config
                .email_from
                .as_deref()
                .ok_or(MailError::InvalidAddress)?,
        )?;
        Ok(Some(Self {
            transport: Transport::Smtp(smtp_transport(smtp)?),
            from,
            templates: templates()?,
        }))
    }

    /// Stub transport. `from` is the `From` mailbox.
    #[cfg(test)]
    pub(crate) fn stub(from: &str) -> Self {
        Self {
            transport: Transport::Stub(Arc::new(std::sync::Mutex::new(StubInbox {
                sent: Vec::new(),
                failures_left: 0,
            }))),
            from: from.parse().expect("stub from address is a mailbox"),
            templates: templates().expect("email templates parse"),
        }
    }

    /// The next `times` sends fail before one succeeds. Ignored for SMTP.
    #[cfg(test)]
    pub(crate) fn fail_next(&self, times: usize) {
        if let Transport::Stub(inbox) = &self.transport {
            lock(inbox).failures_left = times;
        }
    }

    /// Messages this stub accepted. Empty for an SMTP transport.
    #[cfg(test)]
    pub(crate) fn sent(&self) -> Vec<SentMail> {
        match &self.transport {
            Transport::Stub(inbox) => lock(inbox).sent.clone(),
            Transport::Smtp(_) => Vec::new(),
        }
    }

    /// Render `email` to a subject and both parts.
    ///
    /// # Errors
    ///
    /// Returns [`MailError::Template`] when a template is missing or rejects its
    /// context. The message does not include the context.
    pub(crate) fn render(&self, email: &Email) -> Result<Rendered, MailError> {
        let subject = subject(email);
        let (text_name, html_name, context) = match email {
            Email::Invite {
                org_name,
                inviter_name,
                role,
                url,
            } => (
                "invite.txt",
                "invite.html",
                context! {
                    org_name => org_name,
                    inviter_name => inviter_name,
                    role => role_name(*role),
                    url => url,
                },
            ),
            Email::PasswordReset { url } => (
                "password_reset.txt",
                "password_reset.html",
                context! { url => url },
            ),
            Email::VerifyEmail { url } => (
                "verify_email.txt",
                "verify_email.html",
                context! { url => url },
            ),
            Email::Test => ("test.txt", "test.html", context! {}),
        };
        Ok(Rendered {
            subject,
            text: self.render_one(text_name, &context)?,
            html: self.render_one(html_name, &context)?,
        })
    }

    /// Render and send `email` to `to` on this task.
    ///
    /// # Errors
    ///
    /// Returns [`MailError`] for a bad address, a template failure, or an SMTP
    /// failure. The message does not include `to` or the server reply.
    pub(crate) async fn send_now(&self, to: &str, email: &Email) -> Result<(), MailError> {
        self.deliver(to, email)
            .await
            .map(|_| ())
            .map_err(|failure| failure.error)
    }

    /// Send on a background task. Up to three attempts, waiting 1 s then 2 s.
    ///
    /// The log line carries `template`, `message_id`, and `attempt`. A failure
    /// also carries `kind`. The recipient is not logged.
    pub(crate) fn send_in_background(self: &Arc<Self>, to: String, email: Email) {
        let mailer = Arc::clone(self);
        tokio::spawn(async move {
            let template = template_name(&email);
            let mut attempt = 1_u32;
            loop {
                match mailer.deliver(&to, &email).await {
                    Ok(message_id) => {
                        tracing::info!(
                            target: "email",
                            template,
                            message_id,
                            attempt,
                            "email sent"
                        );
                        break;
                    }
                    Err(_) if attempt < ATTEMPTS => {
                        tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
                        attempt += 1;
                    }
                    Err(failure) => {
                        tracing::warn!(
                            target: "email",
                            template,
                            message_id = failure.message_id.as_str(),
                            attempt,
                            kind = failure.error.kind(),
                            "email failed"
                        );
                        break;
                    }
                }
            }
        });
    }

    fn render_one(&self, name: &str, context: &Value) -> Result<String, MailError> {
        self.templates
            .get_template(name)
            .and_then(|template| template.render(context))
            .map_err(|error| MailError::Template(Box::new(error)))
    }

    async fn deliver(&self, to: &str, email: &Email) -> Result<String, Failure> {
        let rendered = self
            .render(email)
            .map_err(|error| Failure::new("-", error))?;
        let message = match build_message(&self.from, to, &rendered) {
            Ok(message) => message,
            Err(error) => return Err(Failure::new("-", error)),
        };
        let message_id = message_id_of(&message);
        #[cfg(test)]
        if let Transport::Stub(inbox) = &self.transport {
            let mut inbox = lock(inbox);
            if inbox.failures_left > 0 {
                inbox.failures_left -= 1;
                return Err(Failure::new(
                    &message_id,
                    MailError::Connect(Box::new(StubFailure)),
                ));
            }
            inbox.sent.push(SentMail {
                to: to.to_owned(),
                subject: rendered.subject.clone(),
                text: rendered.text.clone(),
                html: rendered.html.clone(),
            });
            return Ok(message_id);
        }
        self.transmit(message)
            .await
            .map_err(|error| Failure::new(&message_id, error))?;
        Ok(message_id)
    }

    async fn transmit(&self, message: Message) -> Result<(), MailError> {
        match &self.transport {
            Transport::Smtp(transport) => {
                transport.send(message).await.map_err(classify)?;
                Ok(())
            }
            #[cfg(test)]
            Transport::Stub(_) => Ok(()),
        }
    }
}

impl Failure {
    fn new(message_id: &str, error: MailError) -> Self {
        Self {
            message_id: message_id.to_owned(),
            error,
        }
    }
}

fn mailbox(text: &str) -> Result<Mailbox, MailError> {
    text.parse().map_err(|_| MailError::InvalidAddress)
}

fn smtp_transport(smtp: &SmtpUrl) -> Result<AsyncSmtpTransport<Tokio1Executor>, MailError> {
    let builder = match smtp.tls {
        Tls::Required => {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host).map_err(classify)?
        }
        Tls::Wrapper => {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host).map_err(classify)?
        }
        Tls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host),
    };
    let builder = builder.port(smtp.port);
    let builder = if let Some((user, password)) = &smtp.credentials {
        builder.credentials(Credentials::new(user.clone(), password.expose().clone()))
    } else {
        builder
    };
    Ok(builder.build())
}

fn classify(error: lettre::transport::smtp::Error) -> MailError {
    if error.is_tls() {
        return MailError::Tls(Box::new(error));
    }
    if let Some(code) = error.status() {
        let code = u16::from(code);
        if matches!(code, 530 | 534 | 535) {
            return MailError::Auth(Box::new(error));
        }
        return MailError::Rejected(Box::new(error));
    }
    MailError::Connect(Box::new(error))
}

fn templates() -> Result<Environment<'static>, MailError> {
    let mut environment = Environment::new();
    environment.set_auto_escape_callback(|name| {
        if name.ends_with(".html") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    environment.add_global("product", PRODUCT);
    for (name, source) in TEMPLATES {
        environment
            .add_template(name, source)
            .map_err(|error| MailError::Template(Box::new(error)))?;
    }
    Ok(environment)
}

fn subject(email: &Email) -> String {
    match email {
        Email::Invite { org_name, .. } => format!("You're invited to {org_name} on {PRODUCT}"),
        Email::PasswordReset { .. } => format!("Reset your {PRODUCT} password"),
        Email::VerifyEmail { .. } => format!("Verify your email for {PRODUCT}"),
        Email::Test => format!("{PRODUCT} test email"),
    }
}

fn template_name(email: &Email) -> &'static str {
    match email {
        Email::Invite { .. } => "invite",
        Email::PasswordReset { .. } => "password_reset",
        Email::VerifyEmail { .. } => "verify_email",
        Email::Test => "test",
    }
}

fn role_name(role: OrgRole) -> &'static str {
    match role {
        OrgRole::Owner => "owner",
        OrgRole::Admin => "admin",
        OrgRole::Member => "member",
        OrgRole::Viewer => "viewer",
    }
}

fn build_message(from: &Mailbox, to: &str, rendered: &Rendered) -> Result<Message, MailError> {
    Message::builder()
        .from(from.clone())
        .to(mailbox(to)?)
        .subject(rendered.subject.clone())
        .message_id(None)
        .multipart(MultiPart::alternative_plain_html(
            rendered.text.clone(),
            rendered.html.clone(),
        ))
        .map_err(|_| MailError::InvalidAddress)
}

fn message_id_of(message: &Message) -> String {
    let bytes = message.formatted();
    let formatted = String::from_utf8_lossy(&bytes);
    for line in formatted.lines() {
        let Some(rest) = line.strip_prefix("Message-ID:") else {
            continue;
        };
        return rest.trim().to_owned();
    }
    "-".to_owned()
}

#[cfg(test)]
fn lock(inbox: &std::sync::Mutex<StubInbox>) -> std::sync::MutexGuard<'_, StubInbox> {
    inbox.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::Method;

    use crate::api::account::{AuthConfig, OrgRole};
    use crate::test_support::{TestApp, capture_logs};

    use super::{Email, Mailer};

    fn mailer() -> Mailer {
        Mailer::stub("llmobs <noreply@example.com>")
    }

    fn rendered(mailer: &Mailer, email: &Email) -> String {
        let rendered = mailer.render(email).unwrap();
        format!(
            "Subject: {subject}\n\n{text}\n---\n{html}",
            subject = rendered.subject,
            text = rendered.text,
            html = rendered.html,
        )
    }

    #[test]
    fn templates_render() {
        let mailer = mailer();
        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(
                "invite",
                rendered(
                    &mailer,
                    &Email::Invite {
                        org_name: "Acme".to_owned(),
                        inviter_name: Some("Ada".to_owned()),
                        role: OrgRole::Member,
                        url: "https://example.com/invite/tok".to_owned(),
                    },
                )
            );
            insta::assert_snapshot!(
                "password_reset",
                rendered(
                    &mailer,
                    &Email::PasswordReset {
                        url: "https://example.com/reset?token=tok".to_owned(),
                    },
                )
            );
            insta::assert_snapshot!(
                "verify_email",
                rendered(
                    &mailer,
                    &Email::VerifyEmail {
                        url: "https://example.com/verify?token=tok".to_owned(),
                    },
                )
            );
            insta::assert_snapshot!("test_email", rendered(&mailer, &Email::Test));
        });

        let without = mailer
            .render(&Email::Invite {
                org_name: "Acme".to_owned(),
                inviter_name: None,
                role: OrgRole::Viewer,
                url: "https://example.com/invite/tok".to_owned(),
            })
            .unwrap();
        assert!(without.text.contains("You are invited to join Acme"));
        assert!(without.text.contains("as viewer"));
        assert!(!without.text.contains("invited you"));
        assert!(without.html.contains("as viewer"));
    }

    #[test]
    fn html_escapes_the_org_name() {
        let rendered = mailer()
            .render(&Email::Invite {
                org_name: "<script>alert(1)</script>".to_owned(),
                inviter_name: None,
                role: OrgRole::Admin,
                url: "https://example.com/invite/tok".to_owned(),
            })
            .unwrap();
        assert!(
            rendered.html.contains("&lt;script&gt;"),
            "{}",
            rendered.html
        );
        assert!(!rendered.html.contains("<script>"), "{}", rendered.html);
        assert!(!rendered.html.contains("</script>"), "{}", rendered.html);
        assert!(rendered.text.contains("<script>alert(1)</script>"));
    }

    #[tokio::test]
    async fn stub_records_a_message_and_rejects_a_bad_address() {
        let mailer = mailer();
        mailer
            .send_now("ada@example.com", &Email::Test)
            .await
            .unwrap();
        let sent = mailer.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, "ada@example.com");
        assert_eq!(sent[0].subject, "llmobs test email");
        assert!(sent[0].text.contains("llmobs"));
        assert!(sent[0].html.contains("llmobs"));

        let error = mailer
            .send_now("not-an-email", &Email::Test)
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("not-an-email"), "{error}");
        assert!(!format!("{error:?}").contains("not-an-email"));
        assert_eq!(mailer.sent().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_send_retries_then_logs_the_template() {
        let app = TestApp::with_mailer().await;
        let mailer = Arc::clone(app.state.mailer.as_ref().expect("stub mailer"));
        mailer.fail_next(2);
        let ((), logs) = capture_logs(async {
            mailer.send_in_background("ada@example.com".to_owned(), Email::Test);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
            while app.sent().is_empty() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert_eq!(app.sent().len(), 1, "{logs}");
        assert!(logs.contains("template"), "{logs}");
        assert!(logs.contains("email sent"), "{logs}");
        assert!(logs.contains("message_id="), "{logs}");
        assert!(!logs.contains("message_id=-"), "{logs}");
        assert!(
            logs.contains("attempt=3") || logs.contains("attempt: 3"),
            "{logs}"
        );
        assert!(!logs.contains("ada@example.com"), "{logs}");
    }

    #[tokio::test]
    async fn auth_config_smtp_follows_the_mailer() {
        let app = TestApp::new().await;
        let config: AuthConfig = app
            .request(Method::GET, "/api/auth/config")
            .send()
            .await
            .json();
        assert!(!config.smtp_enabled);

        let app = TestApp::with_mailer().await;
        let config: AuthConfig = app
            .request(Method::GET, "/api/auth/config")
            .send()
            .await
            .json();
        assert!(config.smtp_enabled);
    }
}
