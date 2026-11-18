// Generated from schemas/v1/cleanup-plan-params.schema.json by `pnpm gen:types`. Do not edit.

/**
 * Report-selected identity. Compare before creating a plan; mismatch refuses target_changed. Generation is an inode identity, not a content modification counter.
 */
export type ExpectedIdentity = ExpectedIdentity1 | ExpectedIdentity2

export interface CleanupPlanParams {
  /**
   * @minItems 1
   */
  paths: [string | Target, ...(string | Target)[]]
}
export interface Target {
  path: string
  path_hex?: string
  expect?: ExpectedIdentity
}
/**
 * Report-selected identity. Compare before creating a plan; mismatch refuses target_changed. Generation is an inode identity, not a content modification counter.
 */
export interface ExpectedIdentity1 {
  /**
   * Btrfs filesystem UUID.
   */
  filesystem_uuid: string
  tree: number
  objectid: number
  generation?: number
  filesystem?: 'btrfs'
}
/**
 * Report-selected identity. Compare before creating a plan; mismatch refuses target_changed. Generation is an inode identity, not a content modification counter.
 */
export interface ExpectedIdentity2 {
  /**
   * Btrfs filesystem UUID.
   */
  filesystem_uuid: string
  tree: null
  objectid: number
  generation?: number
  filesystem: 'ext4'
  inode_device: number
}
