PRAGMA foreign_keys = ON;

CREATE TABLE user (
  id TEXT PRIMARY KEY NOT NULL,
  name TEXT NOT NULL,
  email TEXT NOT NULL UNIQUE,
  emailVerified INTEGER NOT NULL,
  image TEXT,
  createdAt INTEGER NOT NULL,
  updatedAt INTEGER NOT NULL
);

CREATE TABLE session (
  id TEXT PRIMARY KEY NOT NULL,
  expiresAt INTEGER NOT NULL,
  token TEXT NOT NULL UNIQUE,
  createdAt INTEGER NOT NULL,
  updatedAt INTEGER NOT NULL,
  ipAddress TEXT,
  userAgent TEXT,
  userId TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE
);

CREATE INDEX session_userId ON session(userId);

CREATE TABLE account (
  id TEXT PRIMARY KEY NOT NULL,
  accountId TEXT NOT NULL,
  providerId TEXT NOT NULL,
  issuer TEXT NOT NULL,
  userId TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
  accessToken TEXT,
  refreshToken TEXT,
  idToken TEXT,
  accessTokenExpiresAt INTEGER,
  refreshTokenExpiresAt INTEGER,
  scope TEXT,
  password TEXT,
  createdAt INTEGER NOT NULL,
  updatedAt INTEGER NOT NULL,
  UNIQUE(issuer, accountId)
);

CREATE INDEX account_userId ON account(userId);

CREATE TABLE verification (
  id TEXT PRIMARY KEY NOT NULL,
  identifier TEXT NOT NULL,
  value TEXT NOT NULL,
  expiresAt INTEGER NOT NULL,
  createdAt INTEGER,
  updatedAt INTEGER
);

CREATE INDEX verification_identifier ON verification(identifier);
