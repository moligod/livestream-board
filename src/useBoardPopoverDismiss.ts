import { useEffect, useRef } from 'react'

export function useBoardPopoverDismiss(open: boolean, close: () => void) {
  const container = useRef<HTMLDivElement>(null)
  const closeRef = useRef(close)

  useEffect(() => { closeRef.current = close }, [close])

  useEffect(() => {
    if (!open) return
    const onPagePointerDown = (event: PointerEvent) => {
      if (event.target instanceof Node && !container.current?.contains(event.target)) closeRef.current()
    }
    const onFramePointerDown = () => closeRef.current()
    const frame = document.querySelector<HTMLIFrameElement>('.board-preview-frame')
    let frameDocument: Document | null = null

    // A click in the same-origin preview does not bubble into the toolbar's document.
    const bindFrame = () => {
      frameDocument?.removeEventListener('pointerdown', onFramePointerDown, true)
      frameDocument = frame?.contentDocument ?? null
      frameDocument?.addEventListener('pointerdown', onFramePointerDown, true)
    }

    document.addEventListener('pointerdown', onPagePointerDown, true)
    frame?.addEventListener('load', bindFrame)
    bindFrame()
    return () => {
      document.removeEventListener('pointerdown', onPagePointerDown, true)
      frame?.removeEventListener('load', bindFrame)
      frameDocument?.removeEventListener('pointerdown', onFramePointerDown, true)
    }
  }, [open])

  return container
}

