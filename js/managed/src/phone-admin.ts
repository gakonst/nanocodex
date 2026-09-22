/** Deployment-only selection of the single phone admin; not an account-wide role. */
export type PhoneAdminConfig = {
  NANOCODEX_PHONE_ADMIN_ID?: string;
  NANOCODEX_PHONE_OWNER_ID?: string;
};

export function phoneAdminConfigured(config: PhoneAdminConfig): boolean {
  return !!config.NANOCODEX_PHONE_ADMIN_ID
    && config.NANOCODEX_PHONE_OWNER_ID === config.NANOCODEX_PHONE_ADMIN_ID;
}
