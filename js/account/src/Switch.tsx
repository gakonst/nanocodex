import * as SwitchPrimitive from "@radix-ui/react-switch";
import {
  forwardRef,
  type ComponentPropsWithoutRef,
  type ElementRef,
} from "react";

export const Switch = forwardRef<
  ElementRef<typeof SwitchPrimitive.Root>,
  ComponentPropsWithoutRef<typeof SwitchPrimitive.Root>
>(function Switch({ className, ...props }, ref) {
  return (
    <SwitchPrimitive.Root
      ref={ref}
      className={className == null ? "commit-switch" : className}
      {...props}
    >
      <SwitchPrimitive.Thumb className="commit-switch-thumb" />
    </SwitchPrimitive.Root>
  );
});
