export type VaultMetadata = Readonly<{
  id: string;
  kind: "login";
  name: string;
  created_at: number;
  username: string;
}> | Readonly<{
  id: string;
  kind: "card";
  name: string;
  created_at: number;
  last4: string;
}> | Readonly<{
  id: string;
  kind: "address";
  name: string;
  created_at: number;
  address_line_1: string;
  address_line_2?: string;
  city: string;
  state: string;
  zip: string;
  country: string;
}> | Readonly<{
  id: string;
  kind: "phone";
  name: string;
  created_at: number;
  phone_number: string;
}>;

export function projectVaultEntries(value: unknown): VaultMetadata[];
