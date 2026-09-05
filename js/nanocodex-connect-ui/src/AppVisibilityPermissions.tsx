import type { VisibilityPermission } from "./connectPolicy.mjs";

export function AppVisibilityPermissions({
  permissions,
}: Readonly<{ permissions: readonly VisibilityPermission[] }>) {
  return permissions.map((permission) => (
    <div key={permission.resource} role="listitem">
      <span>✓</span>
      <div><strong>{permission.label}</strong><small>{permission.detail}</small></div>
    </div>
  ));
}
