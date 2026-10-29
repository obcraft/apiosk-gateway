pub(crate) mod helpers;
mod management;
mod models;
mod registration;

pub use management::{delete_api, my_apis, update_api, update_endpoint_docs};
pub use models::{
    CommunityApiInfo, DeleteApiQuery, DeleteApiResponse, MyApisQuery, MyApisResponse,
    RegisterApiRequest, RegisterApiResponse, UpdateApiRequest, UpdateApiResponse,
    UpdateEndpointDocsRequest, UpdateEndpointDocsResponse,
};
pub use registration::register_api;

#[cfg(test)]
mod tests {
    use super::helpers::{registration_message, registration_status};

    #[test]
    fn unverified_registration_is_inactive() {
        assert_eq!(registration_status(false), "inactive");
        assert!(registration_message(false).contains("inactive"));
    }

    #[test]
    fn health_check_pass_is_active_but_awaits_verification() {
        assert_eq!(registration_status(true), "active");
        let msg = registration_message(true).to_lowercase();
        assert!(
            msg.contains("verification") || msg.contains("verified"),
            "health-pass message must mention pending Apiosk verification: {msg}"
        );
    }
}
