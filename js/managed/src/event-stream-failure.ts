export type EventStreamFailurePersistence<Event> = Readonly<{
  event?: Event;
  fenceError?: unknown;
  noticeError?: unknown;
}>;

type EventStreamFailureStorage = Pick<DurableObjectStorage, "sql" | "transactionSync">;

/**
 * Persists the replay fence before attempting the optional replay notice.
 *
 * The notice allocates another event row and can fail for the same reason as
 * the projection that tripped the fence. Keeping it in a later transaction
 * ensures that failure cannot roll back the authoritative session state.
 */
export function persistEventStreamFailure<Event>(
  storage: EventStreamFailureStorage,
  detail: string,
  now: number,
  appendNotice: () => Event,
): EventStreamFailurePersistence<Event> {
  try {
    storage.sql.exec(
      "UPDATE session_state SET stream_error = ?, last_active = ? WHERE singleton = 1",
      detail,
      now,
    );
  } catch (fenceError) {
    return { fenceError };
  }

  try {
    return { event: storage.transactionSync(appendNotice) };
  } catch (noticeError) {
    return { noticeError };
  }
}
