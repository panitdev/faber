// The bare harness: passes its input straight to the model, appended to
// whatever history.read() hands back. §4's "default is identity" — this is
// the literal claim made paste-able as code.
//
// `input` is the turn's messages, plural: usually just the user's, sometimes
// theirs plus something the consumer had to say in the conversation rather
// than in the prefix.
export default {
  execute: (ctx, input) =>
    ctx.llm.stream({ messages: [...ctx.history.read(), ...input] }),
};
