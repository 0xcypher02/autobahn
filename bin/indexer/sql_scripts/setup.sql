CREATE SCHEMA IF NOT EXISTS router AUTHORIZATION CURRENT_ROLE;
CREATE TABLE IF NOT EXISTS router.tx_history
(
    signature VARCHAR(88) NOT NULL,
    timestamp TIMESTAMP WITH TIME ZONE NOT NULL,
    is_success BOOLEAN NOT NULL,
    router_version INT NOT NULL
);