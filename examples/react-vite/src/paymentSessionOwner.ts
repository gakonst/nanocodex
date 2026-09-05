export type CloseablePaymentSession = object & {
  mpp: {
    close(): unknown | Promise<unknown>;
  };
};

/** Owns one caller-managed MPP session across agent replacements. */
export function createPaymentSessionOwner<
  Session extends CloseablePaymentSession,
>() {
  let current: Session | undefined;
  const closed = new WeakSet<Session>();

  async function closeOnce(session: Session): Promise<void> {
    if (closed.has(session)) return;
    closed.add(session);
    await session.mpp.close();
  }

  async function clear(): Promise<void> {
    const previous = current;
    current = undefined;
    if (previous) await closeOnce(previous);
  }

  async function open<Result>(
    create: () => Promise<Session>,
    use: (session: Session) => Promise<Result>,
  ): Promise<Result> {
    await clear();
    const session = await create();
    current = session;
    try {
      return await use(session);
    } catch (error) {
      current = undefined;
      try {
        await closeOnce(session);
      } catch (closeError) {
        throw new AggregateError(
          [error, closeError],
          "Agent creation and MPP session cleanup both failed",
        );
      }
      throw error;
    }
  }

  return Object.freeze({ clear, open });
}
