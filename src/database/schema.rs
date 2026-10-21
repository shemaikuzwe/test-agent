// @generated automatically by Diesel CLI.

diesel::table! {
    jobs (deployment_id) {
        deployment_id -> Text,
        node_id -> Text,
        state -> Text,
        error -> Nullable<Text>,
        updated_at -> BigInt,
        created_at -> BigInt,
        image -> Text,
        application_id -> Text,
        port -> Nullable<Integer>,
        hostname -> Nullable<Text>,
    }
}
