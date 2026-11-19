package com.sednalabs.mcp.acceptance;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.res.Configuration;
import android.graphics.Color;
import android.os.Bundle;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/** Disposable, offline UI fixture. No permissions, account data or network use. */
public final class MainActivity extends Activity {
    private static final String CLIPBOARD_SENTINEL = "sednalabs-acceptance-clipboard-sentinel";
    private int count;
    private int maxPointers;
    private int completedTwoPointerGestures;
    private boolean gestureActive;
    private boolean sawTwoPointers;
    private boolean gesturePositionsValid;
    private boolean controlsVisible;
    private String text = "";
    private EditText input;
    private TextView counter;
    private TextView pointerCount;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
        clipboard.setPrimaryClip(ClipData.newPlainText("Android MCP acceptance", CLIPBOARD_SENTINEL));
        showControls();
    }

    @Override public void onConfigurationChanged(Configuration configuration) {
        super.onConfigurationChanged(configuration);
        if (controlsVisible) {
            text = input.getText().toString();
            showControls();
        }
    }

    private boolean clipboardPreserved() {
        ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
        ClipData clip = clipboard.getPrimaryClip();
        return clip != null && clip.getItemCount() == 1
            && clip.getItemAt(0).getText() != null
            && CLIPBOARD_SENTINEL.contentEquals(clip.getItemAt(0).getText());
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private LinearLayout page() {
        LinearLayout page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setPadding(dp(24), dp(28), dp(24), dp(20));
        page.setFitsSystemWindows(true);
        page.setBackgroundColor(Color.WHITE);
        page.setFocusableInTouchMode(true);
        return page;
    }

    private TextView label(String text) {
        TextView label = new TextView(this);
        label.setText(text);
        label.setTextSize(18);
        label.setTextColor(Color.BLACK);
        label.setPadding(0, dp(6), 0, dp(6));
        return label;
    }

    private Button button(int id, String text, View.OnClickListener listener) {
        Button button = new Button(this);
        button.setId(id);
        button.setText(text);
        button.setAllCaps(false);
        button.setOnClickListener(listener);
        return button;
    }

    private void showControls() {
        controlsVisible = true;
        LinearLayout page = page();
        LinearLayout controls = page;
        LinearLayout touchArea = page;
        if (getResources().getConfiguration().orientation == Configuration.ORIENTATION_LANDSCAPE) {
            // Keep the complete touch pad visible in the shorter landscape viewport.
            page.setOrientation(LinearLayout.HORIZONTAL);
            page.setPadding(dp(16), dp(12), dp(16), dp(12));
            controls = new LinearLayout(this);
            controls.setOrientation(LinearLayout.VERTICAL);
            touchArea = new LinearLayout(this);
            touchArea.setOrientation(LinearLayout.VERTICAL);
            touchArea.setPadding(dp(16), 0, 0, 0);
            page.addView(controls, new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.MATCH_PARENT, 1));
            page.addView(touchArea, new LinearLayout.LayoutParams(0,
                LinearLayout.LayoutParams.MATCH_PARENT, 1));
        }
        controls.addView(label("Android MCP acceptance"));
        counter = label("Count: " + count);
        counter.setId(R.id.counter);
        controls.addView(counter);
        controls.addView(button(R.id.increment, "Increment", view -> {
            count++;
            counter.setText("Count: " + count);
        }));
        input = new EditText(this);
        input.setId(R.id.text_input);
        input.setContentDescription("Acceptance text input");
        input.setSingleLine(true);
        input.setHint("Enter test text");
        input.setText(text);
        controls.addView(input);
        controls.addView(button(R.id.dialog, "Open dialog", view ->
            new AlertDialog.Builder(this).setTitle("Acceptance dialog")
                .setMessage(clipboardPreserved() ? "Clipboard preserved" : "Clipboard changed")
                .setPositiveButton("Close dialog", (dialog, which) -> dialog.dismiss())
                .show()));
        controls.addView(button(R.id.scroll, "Open scroll page", view -> {
            text = input.getText().toString();
            showScrollPage();
        }));
        pointerCount = label("Max pointers: " + maxPointers);
        pointerCount.setId(R.id.pointer_count);
        pointerCount.setContentDescription("Completed two-pointer gestures: " + completedTwoPointerGestures);
        touchArea.addView(pointerCount);
        TextView pad = label("Touch pad");
        pad.setId(R.id.touch_pad);
        pad.setGravity(Gravity.CENTER);
        pad.setContentDescription("Acceptance touch pad");
        pad.setBackgroundColor(Color.rgb(220, 235, 245));
        pad.setOnTouchListener((view, event) -> {
            int action = event.getActionMasked();
            if (action == MotionEvent.ACTION_DOWN) {
                maxPointers = 0;
                gestureActive = true;
                sawTwoPointers = false;
                gesturePositionsValid = true;
            }
            maxPointers = Math.max(maxPointers, event.getPointerCount());
            for (int pointer = 0; pointer < event.getPointerCount(); pointer++) {
                float x = event.getX(pointer);
                float y = event.getY(pointer);
                gesturePositionsValid &= x >= 0 && x < view.getWidth()
                    && y >= 0 && y < view.getHeight();
            }
            if (event.getPointerCount() == 2) {
                sawTwoPointers = true;
                // Asymmetric target regions prove both pointers reached this pad.
                // Merely receiving two pointers elsewhere or two independent taps
                // cannot advance the completed-gesture count.
                gesturePositionsValid &= (inTouchRegion(view, event, 0, true)
                    && inTouchRegion(view, event, 1, false))
                    || (inTouchRegion(view, event, 1, true)
                    && inTouchRegion(view, event, 0, false));
            }
            if (action == MotionEvent.ACTION_UP) {
                if (gestureActive && maxPointers == 2 && sawTwoPointers && gesturePositionsValid) {
                    completedTwoPointerGestures++;
                }
                gestureActive = false;
            } else if (action == MotionEvent.ACTION_CANCEL) {
                gestureActive = false;
            }
            pointerCount.setText("Max pointers: " + maxPointers);
            pointerCount.setContentDescription("Completed two-pointer gestures: " + completedTwoPointerGestures);
            return true;
        });
        touchArea.addView(pad, new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, dp(150)));
        setContentView(page);
        page.requestFocus();
    }

    private boolean inTouchRegion(View view, MotionEvent event, int pointer, boolean left) {
        float x = event.getX(pointer) / view.getWidth();
        float y = event.getY(pointer) / view.getHeight();
        return left ? x >= 0.1f && x <= 0.45f && y >= 0.2f && y <= 0.5f
            : x >= 0.55f && x <= 0.9f && y >= 0.5f && y <= 0.8f;
    }

    private void showScrollPage() {
        controlsVisible = false;
        ScrollView scroll = new ScrollView(this);
        scroll.setFillViewport(true);
        LinearLayout page = page();
        page.addView(label("Scroll acceptance page"));
        for (int i = 1; i <= 18; i++) {
            TextView row = label("Scroll row " + i);
            row.setMinHeight(dp(64));
            page.addView(row);
        }
        TextView end = label("Scroll destination reached");
        end.setId(R.id.scroll_end);
        page.addView(end);
        page.addView(button(R.id.back_to_controls, "Back to controls", view -> showControls()));
        scroll.addView(page);
        setContentView(scroll);
    }
}
