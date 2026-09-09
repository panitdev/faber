// @generated automatically by Diesel CLI.

diesel::table! {
    agent_credential (id) {
        id -> Uuid,
        host_id -> Uuid,
        token_hash -> Text,
        host_pubkey -> Text,
        issued_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    agent_enrollment (id) {
        id -> Uuid,
        host_id -> Uuid,
        token_hash -> Text,
        expires_at -> Timestamptz,
        consumed_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    credentials (id) {
        id -> Uuid,
        user_id -> Uuid,
        label -> Text,
        kind -> Text,
        key_ciphertext -> Bytea,
        key_nonce -> Bytea,
        key_version -> Text,
        last_four -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    models (id) {
        id -> Uuid,
        user_id -> Uuid,
        alias -> Text,
        base_url -> Text,
        wire -> Text,
        wire_id -> Text,
        family -> Nullable<Text>,
        credential_id -> Nullable<Uuid>,
        params -> Jsonb,
        capabilities -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    host (id) {
        id -> Uuid,
        user_id -> Uuid,
        name -> Text,
        transport -> Text,
        exec_mode -> Text,
        ssh_address -> Nullable<Text>,
        ssh_key_ref -> Nullable<Text>,
        ssh_host_key -> Nullable<Text>,
        docker_endpoint -> Nullable<Text>,
        created_at -> Timestamptz,
        disabled_at -> Nullable<Timestamptz>,
        root_path -> Nullable<Text>,
        preview_network -> Nullable<Text>,
    }
}

diesel::table! {
    presentation (id) {
        id -> Uuid,
        session_id -> Uuid,
        environment_label -> Text,
        port -> Int4,
        token -> Text,
        upstream_host_mode -> Text,
        created_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    host_container (id) {
        id -> Uuid,
        host_id -> Uuid,
        container_ref -> Text,
        name -> Nullable<Text>,
        root_path -> Text,
        created_at -> Timestamptz,
        unregistered_at -> Nullable<Timestamptz>,
        managed_at -> Nullable<Timestamptz>,
        image_id -> Nullable<Uuid>,
        user_id -> Uuid,
    }
}

diesel::table! {
    host_probe (id) {
        id -> Uuid,
        host_id -> Uuid,
        container_id -> Nullable<Uuid>,
        probed_at -> Timestamptz,
        ok -> Bool,
        error -> Nullable<Text>,
        os -> Nullable<Text>,
        arch -> Nullable<Text>,
        shell -> Nullable<Text>,
        tools -> Nullable<Jsonb>,
        root_path -> Nullable<Text>,
    }
}

diesel::table! {
    image (id) {
        id -> Uuid,
        user_id -> Uuid,
        name -> Text,
        reference -> Text,
        default_mounts -> Nullable<Jsonb>,
        default_root_path -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    blob (digest) {
        digest -> Bytea,
        data -> Nullable<Bytea>,
        storage_path -> Nullable<Text>,
        byte_length -> Int8,
        refcount -> Int8,
        created_at -> Int8,
    }
}

diesel::table! {
    exchange (id) {
        id -> Uuid,
        run_id -> Uuid,
        request_blob_digest -> Bytea,
        provider_events_digest -> Nullable<Bytea>,
        canonical_blob_digest -> Nullable<Bytea>,
        usage -> Nullable<Jsonb>,
        outcome -> Nullable<Jsonb>,
        expected_cache_tokens -> Int8,
        actual_cache_tokens -> Nullable<Int8>,
        started_at -> Int8,
        completed_at -> Nullable<Int8>,
    }
}

diesel::table! {
    run (id) {
        id -> Uuid,
        thread_id -> Uuid,
        created_at -> Int8,
        completed_at -> Nullable<Int8>,
    }
}

diesel::table! {
    session (id) {
        id -> Uuid,
        workspace_id -> Uuid,
        title -> Nullable<Text>,
        created_at -> Int8,
        closed_at -> Nullable<Int8>,
        model_alias -> Nullable<Text>,
        thinking_effort -> Nullable<Text>,
    }
}

diesel::table! {
    session_environment (session_id, label) {
        session_id -> Uuid,
        label -> Text,
        host_id -> Uuid,
        container_id -> Nullable<Uuid>,
        added_at -> Int8,
        removed_at -> Nullable<Int8>,
    }
}

diesel::table! {
    session_ref (token) {
        token -> Text,
        session_id -> Uuid,
        issued_at -> Int8,
        revoked_at -> Nullable<Int8>,
    }
}

diesel::table! {
    span (id) {
        id -> Uuid,
        exchange_id -> Uuid,
        start_offset -> Int8,
        end_offset -> Int8,
        created_at -> Int8,
    }
}

diesel::table! {
    spine (thread_id, seq) {
        thread_id -> Uuid,
        seq -> Int8,
        exchange_id -> Uuid,
        explicit_commit -> Bool,
        created_at -> Int8,
    }
}

diesel::table! {
    thread (id) {
        id -> Uuid,
        session_id -> Uuid,
        parent_id -> Nullable<Uuid>,
        forked_at_seq -> Nullable<Int4>,
        next_seq -> Int4,
        created_at -> Int8,
    }
}

diesel::table! {
    transcript (id) {
        id -> Uuid,
        run_id -> Uuid,
        seq -> Int8,
        kind -> Text,
        payload -> Jsonb,
        created_at -> Int8,
    }
}

diesel::table! {
    users (id) {
        id -> Uuid,
        identity_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    workspace (id) {
        id -> Uuid,
        kind -> Text,
        user_id -> Nullable<Uuid>,
        created_at -> Int8,
    }
}

diesel::table! {
    workspace_member (workspace_id, user_id) {
        workspace_id -> Uuid,
        user_id -> Uuid,
        role -> Text,
        joined_at -> Int8,
    }
}

diesel::joinable!(agent_credential -> host (host_id));
diesel::joinable!(agent_enrollment -> host (host_id));
diesel::joinable!(credentials -> users (user_id));
diesel::joinable!(exchange -> run (run_id));
diesel::joinable!(host -> users (user_id));
diesel::joinable!(host_container -> host (host_id));
diesel::joinable!(host_container -> image (image_id));
diesel::joinable!(host_container -> users (user_id));
diesel::joinable!(host_probe -> host (host_id));
diesel::joinable!(host_probe -> host_container (container_id));
diesel::joinable!(image -> users (user_id));
diesel::joinable!(models -> users (user_id));
diesel::joinable!(models -> credentials (credential_id));
diesel::joinable!(presentation -> session (session_id));
diesel::joinable!(run -> thread (thread_id));
diesel::joinable!(session -> workspace (workspace_id));
diesel::joinable!(session_environment -> session (session_id));
diesel::joinable!(session_environment -> host (host_id));
diesel::joinable!(session_environment -> host_container (container_id));
diesel::joinable!(session_ref -> session (session_id));
diesel::joinable!(span -> exchange (exchange_id));
diesel::joinable!(spine -> exchange (exchange_id));
diesel::joinable!(spine -> thread (thread_id));
diesel::joinable!(thread -> session (session_id));
diesel::joinable!(transcript -> run (run_id));
diesel::joinable!(workspace -> users (user_id));
diesel::joinable!(workspace_member -> users (user_id));
diesel::joinable!(workspace_member -> workspace (workspace_id));

diesel::allow_tables_to_appear_in_same_query!(
    agent_credential,
    agent_enrollment,
    blob,
    credentials,
    exchange,
    host,
    host_container,
    host_probe,
    image,
    models,
    presentation,
    run,
    session,
    session_environment,
    session_ref,
    span,
    spine,
    thread,
    transcript,
    users,
    workspace,
    workspace_member,
);
