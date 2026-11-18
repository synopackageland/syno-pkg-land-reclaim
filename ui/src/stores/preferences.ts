// What this browser's viewer chose in Settings.
import { reactive } from 'vue'
import { readLocal, writeLocal } from './storage'

export const SYSTEM_PARTITION_WARNING_KEY = 'synoreclaim.settings.systemPartitionWarning'

/** systemPartitionWarning: Overview's banner and Full row; on unless turned off. */
export const preferences = reactive({ systemPartitionWarning: true })

/** Reads the stored choices again; anything but `false` keeps the warning on. */
export function readPreferences() {
  preferences.systemPartitionWarning = readLocal<unknown>(SYSTEM_PARTITION_WARNING_KEY, true) !== false
}

export function setSystemPartitionWarning(shown: boolean) {
  preferences.systemPartitionWarning = shown
  writeLocal(SYSTEM_PARTITION_WARNING_KEY, shown)
}

readPreferences()
